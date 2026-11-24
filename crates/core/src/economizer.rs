use crate::economizer_rules::{infer_active_tools, match_micro_skill};
use crate::types::{Complexity, NormalizedRequest, SessionState, SlmRouteDecision};
use serde_json::Value;
use xxhash_rust::xxh3::xxh3_64;

const COMPILED_SLM_THRESHOLD: u32 = 3;

/// TokenEconomizer governance:
/// 1. Prefix-safe RoPE tail soft tool-gating (never mutates `tools[]`).
/// 2. Procedural Micro-Skill injection (20-50 tokens, evicted on next turn).
/// 3. XXH3 Failure-Loop Circuit Breaker (`>= 2x` identical action/error -> DEEP + collapse).
/// 4. Compiled SLM routing for stabilized recurring sub-workflows.
pub fn apply_governance(ir: &mut NormalizedRequest, state: &mut SessionState) {
    // Evict previous turn's micro-skill before evaluating current turn.
    state.active_micro_skill = None;

    let last_text = extract_last_message_text(&ir.root);
    let action_sig = extract_last_tool_call_sig(&ir.root);
    let mut had_error = false;

    if let Some(sig) = action_sig {
        let repeated_action = state.recent_action_sigs.last().copied() == Some(sig);
        push_bounded(&mut state.recent_action_sigs, sig, 8);
        if repeated_action {
            trip_circuit_breaker(ir, "identical tool invocation repeated >=2x");
            had_error = true;
        }
    }

    if let Some(text) = last_text.as_deref() {
        if check_error_loop(text, ir, state) {
            had_error = true;
        }
        if let Some((skill_id, directive)) = match_micro_skill(text) {
            state.active_micro_skill = Some(skill_id.to_owned());
            ir.stage_stats.micro_skill_injected = Some(skill_id.to_owned());
            ir.tail_directives.push(directive.to_owned());
        }
    }

    if ir.active_tool_loop {
        let inferred = infer_active_tools(&ir.root, last_text.as_deref().unwrap_or_default());
        let scope = if inferred.is_empty() {
            "minimal required tools".to_owned()
        } else {
            inferred.join(", ")
        };
        ir.tail_directives.push(format!(
            "[Active Tool Scope: {scope} only; ignore all other tools]"
        ));
    }

    if !had_error && ir.complexity == Complexity::Trivial {
        evaluate_compiled_slm_route(ir, state, action_sig);
    }
}

fn check_error_loop(text: &str, ir: &mut NormalizedRequest, state: &mut SessionState) -> bool {
    let is_error = text.contains("Error:")
        || text.contains("Traceback")
        || text.contains("panic:")
        || text.contains("FAILED")
        || text.contains("error[E");
    if !is_error {
        return false;
    }

    // Hard Invariant 6: Never starve debugging.
    ir.complexity = Complexity::Deep;
    let head = &text.as_bytes()[..text.len().min(256)];
    let sig = xxh3_64(head);
    let repeated = state.recent_error_sigs.last().copied() == Some(sig);
    push_bounded(&mut state.recent_error_sigs, sig, 8);

    if repeated {
        trip_circuit_breaker(ir, "identical error signature repeated >=2x");
        collapse_duplicate_retry_turn(ir, state);
    }
    true
}

fn trip_circuit_breaker(ir: &mut NormalizedRequest, reason: &str) {
    ir.complexity = Complexity::Deep;
    ir.stage_stats.circuit_breaker_tripped = true;
    ir.tail_directives.push(format!(
        "[Circuit Breaker: {reason}. Stop retrying the same edit; inspect definitions and pivot strategy.]"
    ));
}

/// Atomically removes the previous duplicate `(assistant tool_call/tool_use + corresponding
/// tool/tool_result)` pair so strict OpenAI & Anthropic message-graph contracts remain 100% legal.
fn collapse_duplicate_retry_turn(ir: &mut NormalizedRequest, state: &SessionState) {
    if state.sacred_cache_boundary > 0 {
        return;
    }
    let Some(messages) = ir.root.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    if messages.len() < 5 {
        return;
    }

    // Current turn is typically `[..., prev_asst, prev_tool_or_user, curr_asst, curr_tool_or_user]`.
    let prev_res_idx = messages.len() - 3;
    let prev_asst_idx = messages.len() - 4;

    let prev_asst_role = messages[prev_asst_idx].get("role").and_then(Value::as_str);
    let prev_res_role = messages[prev_res_idx].get("role").and_then(Value::as_str);

    let is_openai_pair = prev_asst_role == Some("assistant")
        && messages[prev_asst_idx]
            .get("tool_calls")
            .and_then(Value::as_array)
            .map(|a| a.len() == 1)
            .unwrap_or(false)
        && prev_res_role == Some("tool");

    let is_anthropic_pair = prev_asst_role == Some("assistant")
        && prev_res_role == Some("user")
        && messages[prev_asst_idx]
            .get("content")
            .and_then(Value::as_array)
            .map(|a| a.iter().any(|b| b.get("type").and_then(Value::as_str) == Some("tool_use")))
            .unwrap_or(false)
        && messages[prev_res_idx]
            .get("content")
            .and_then(Value::as_array)
            .map(|a| a.iter().any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result")))
            .unwrap_or(false);

    if is_openai_pair || is_anthropic_pair {
        messages.drain(prev_asst_idx..=prev_res_idx);
    }
}

fn evaluate_compiled_slm_route(
    ir: &mut NormalizedRequest,
    state: &mut SessionState,
    action_sig: Option<u64>,
) {
    let Some(sig) = action_sig else {
        return;
    };
    let count = state.workflow_counts.entry(sig).or_insert(0);
    *count += 1;
    if *count >= COMPILED_SLM_THRESHOLD {
        ir.slm_route = SlmRouteDecision::CompiledSlm {
            workflow_id: format!("wf_{sig:016x}"),
            endpoint: "http://127.0.0.1:8081/v1".to_owned(),
        };
    }
}

/// Validates local compiled SLM JSON output; on schema failure caller falls back to frontier & DEEP.
pub fn validate_slm_output(raw: &[u8]) -> bool {
    let Ok(val) = serde_json::from_slice::<Value>(raw) else {
        return false;
    };
    val.get("choices").is_some() || val.get("content").is_some()
}

fn extract_last_message_text(root: &Value) -> Option<String> {
    let last = root.get("messages")?.as_array()?.last()?;
    if let Some(s) = last.get("content").and_then(Value::as_str) {
        return Some(s.to_owned());
    }
    let blocks = last.get("content")?.as_array()?;
    let joined: Vec<&str> = blocks
        .iter()
        .filter_map(|b| b.get("content").or_else(|| b.get("text")).and_then(Value::as_str))
        .collect();
    if joined.is_empty() {
        None
    } else {
        Some(joined.join("\n"))
    }
}

fn extract_last_tool_call_sig(root: &Value) -> Option<u64> {
    let messages = root.get("messages")?.as_array()?;
    for msg in messages.iter().rev().take(3) {
        if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
            if let Some(call) = calls.last() {
                let name = call
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let args = call
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let mut buf = Vec::with_capacity(name.len() + args.len() + 1);
                buf.extend_from_slice(name.as_bytes());
                buf.push(b'|');
                buf.extend_from_slice(args.as_bytes());
                return Some(xxh3_64(&buf));
            }
        }
    }
    None
}

fn push_bounded(vec: &mut Vec<u64>, val: u64, max: usize) {
    vec.push(val);
    if vec.len() > max {
        vec.remove(0);
    }
}
