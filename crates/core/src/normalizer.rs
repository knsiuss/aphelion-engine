pub use crate::ingress_anthropic::has_cache_control;
use crate::ingress_anthropic::{find_last_cache_control_end, find_last_cached_msg_idx};
use crate::types::{
    Complexity, Dialect, NormalizedRequest, SessionState, SlmRouteDecision, StageStats,
    WorkloadClass,
};
use crate::workload::{classify_workload, detect_dialect_universal};
use crate::CoreError;
use serde_json::Value;
use xxhash_rust::xxh3::{xxh3_128, xxh3_64};

/// Parses raw request bytes into a normalized IR across all major LLM dialects.
/// Unrecognized JSON schemas or non-JSON bodies degrade safely to `Dialect::Passthrough`
/// (byte-for-byte untouched) with zero errors.
pub fn normalize(
    raw_json: &[u8],
    state: &mut SessionState,
) -> Result<NormalizedRequest, CoreError> {
    let Ok(root) = serde_json::from_slice::<Value>(raw_json) else {
        return Ok(passthrough_ir(raw_json));
    };
    let dialect = detect_dialect_universal(&root);
    if dialect == Dialect::Passthrough {
        return Ok(passthrough_ir(raw_json));
    }

    let system_has_cache_control = root.get("system").map(has_cache_control).unwrap_or(false)
        || root.get("tools").map(has_cache_control).unwrap_or(false);
    let last_cached_msg_idx = find_last_cached_msg_idx(&root);
    let sacred_boundary = if system_has_cache_control || last_cached_msg_idx.is_some() {
        find_last_cache_control_end(raw_json)
    } else {
        0
    };
    let model = root
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let derived_id = derive_two_phase_session(&root, dialect, state);
    let model_changed = !state.active_model.is_empty() && state.active_model != model;
    let session_changed = state.session_id.is_some_and(|prev| prev != derived_id);
    if session_changed || model_changed {
        state.session_id = Some(derived_id);
        state.active_model = model.clone();
        let key = state.ledger_key(&model);
        if let Some(rec) = state.prefix_ledger.get(&key) {
            state.frozen_prefix_bytes = rec.frozen_prefix_bytes.clone();
            state.frozen_prefix_len = rec.frozen_prefix_bytes.len();
        } else {
            state.frozen_prefix_bytes.clear();
            state.frozen_prefix_len = 0;
            state.sacred_cache_boundary = 0;
            state.system_has_cache_control = false;
            state.last_cached_msg_idx = None;
        }
    } else {
        state.session_id = Some(derived_id);
        state.active_model = model.clone();
    }
    let key_now = state.ledger_key(&model);
    let has_committed = state
        .prefix_ledger
        .get(&key_now)
        .is_some_and(|r| r.committed_len > 0 || !r.frozen_prefix_bytes.is_empty());
    let is_turn_zero = !has_committed && (state.turn_index == 0 || session_changed || model_changed);
    let supports_prefill = model_supports_prefill(&model);
    let supports_vision = model_supports_vision(&model);
    let pre_compaction_messages = root.get("messages").cloned();

    let active_tool_loop = detect_active_tool_loop(&root, dialect);
    let workload = classify_workload(dialect, &root, active_tool_loop);

    Ok(NormalizedRequest {
        dialect,
        workload,
        model_id: model,
        root,
        raw_passthrough: None,
        sacred_boundary,
        system_has_cache_control,
        last_cached_msg_idx,
        is_turn_zero,
        ingress_start: 0,
        active_tool_loop,
        complexity: Complexity::Trivial,
        supports_prefill,
        supports_vision,
        tail_directives: Vec::new(),
        assistant_prefill: None,
        slm_route: SlmRouteDecision::Frontier,
        stage_stats: StageStats::default(),
        pre_compaction_messages,
    })
}

fn passthrough_ir(raw: &[u8]) -> NormalizedRequest {
    NormalizedRequest {
        dialect: Dialect::Passthrough,
        workload: WorkloadClass::Passthrough,
        model_id: String::new(),
        root: Value::Null,
        raw_passthrough: Some(raw.to_vec()),
        sacred_boundary: 0,
        system_has_cache_control: false,
        last_cached_msg_idx: None,
        is_turn_zero: false,
        ingress_start: 0,
        active_tool_loop: false,
        complexity: Complexity::Trivial,
        supports_prefill: false,
        supports_vision: false,
        tail_directives: Vec::new(),
        assistant_prefill: None,
        slm_route: SlmRouteDecision::Frontier,
        stage_stats: StageStats::default(),
        pre_compaction_messages: None,
    }
}

fn derive_two_phase_session(root: &Value, dialect: Dialect, state: &mut SessionState) -> u128 {
    let messages = root.get("messages").and_then(Value::as_array);
    if state.turn_alias.is_none() {
        if let Some(msgs) = messages {
            if let Some(first_asst) = msgs
                .iter()
                .find(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
            {
                let slice = first_asst.to_string();
                state.turn_alias = Some(xxh3_64(&slice.as_bytes()[..slice.len().min(256)]));
            }
        }
    }

    let first_user = messages
        .and_then(|msgs| {
            msgs.iter()
                .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
                .and_then(|m| {
                    m.get("content")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .or_else(|| m.get("content").map(Value::to_string))
                })
        })
        .or_else(|| {
            root.get("input")
                .and_then(Value::as_array)
                .and_then(|arr| arr.first())
                .and_then(|m| m.get("content").map(Value::to_string))
        })
        .or_else(|| root.get("input").map(Value::to_string))
        .or_else(|| root.get("contents").map(Value::to_string))
        .or_else(|| root.get("prompt").map(Value::to_string))
        .unwrap_or_default();

    let sys = match dialect {
        Dialect::OpenAiResponses => root.get("instructions"),
        Dialect::GeminiGenerate => root.get("systemInstruction"),
        _ => root.get("system"),
    }
    .map(Value::to_string)
    .unwrap_or_default();
    let workspace_fp = extract_workspace_fingerprint(&sys, &first_user);
    let end = first_user.len().min(512);
    let mut seed = Vec::with_capacity(workspace_fp.len() + end + 20);
    seed.extend_from_slice(&state.request_options.tenant_hash.to_le_bytes());
    seed.push(b'|');
    seed.extend_from_slice(workspace_fp.as_bytes());
    seed.push(b'|');
    seed.extend_from_slice(&first_user.as_bytes()[..end]);
    xxh3_128(&seed)
}

pub fn extract_workspace_fingerprint(system: &str, first_user: &str) -> String {
    for source in [system, first_user] {
        for marker in [
            "cwd:",
            "CWD:",
            "workspace:",
            "repo_root:",
            "Working directory:",
        ] {
            if let Some(idx) = source.find(marker) {
                let rest = &source[idx + marker.len()..];
                let line = rest
                    .split(['\n', '\r', '"', '}', ';'])
                    .next()
                    .unwrap_or_default()
                    .trim();
                if !line.is_empty() {
                    return line.to_owned();
                }
            }
        }
    }
    String::new()
}

fn detect_active_tool_loop(root: &Value, dialect: Dialect) -> bool {
    let has_tools = root
        .get("tools")
        .and_then(Value::as_array)
        .map(|t| !t.is_empty())
        .unwrap_or(false);
    if !has_tools || root.get("tool_choice").and_then(Value::as_str) == Some("none") {
        return false;
    }
    if dialect == Dialect::OpenAiResponses {
        return true;
    }
    let Some(last) = root
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|m| m.last())
    else {
        return has_tools;
    };
    let role = last.get("role").and_then(Value::as_str).unwrap_or_default();
    if role == "tool" {
        return true;
    }
    if let Some(blocks) = last.get("content").and_then(Value::as_array) {
        if blocks
            .iter()
            .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
        {
            return true;
        }
    }
    role == "user" && has_tools
}

fn model_supports_prefill(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    !(m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") || m.starts_with("gpt-5"))
}

fn model_supports_vision(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m.contains("claude-3")
        || m.contains("claude-sonnet-4")
        || m.contains("claude-opus-4")
        || m.contains("gpt-4o")
        || m.contains("gpt-4.1")
        || m.contains("gemini")
}
