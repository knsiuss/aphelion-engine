use crate::types::{Dialect, NormalizedRequest, RequestOptions, WorkloadClass};
use serde_json::{json, Value};

const CONCISE_OUTPUT_DIRECTIVE: &str = "\n[Output Policy: Emit direct code, diffs, and tool calls without conversational preamble, post-code recaps, or restating the prompt; preserve 100% code completeness.]";

/// Pre-Freeze Output Steering:
/// For string `system`, executes on Turn 0 BEFORE `PrefixLedger` freezes the root prefix.
/// For Anthropic array `system`, appends `[Output Policy: ...]` consistently on EVERY turn
/// after existing cached blocks so `system` stays 100% byte-identical across Turn 0..N.
pub fn inject_concise_output_directive(ir: &mut NormalizedRequest, opts: &RequestOptions) {
    if opts.output_verbose
        || matches!(
            ir.workload,
            WorkloadClass::Passthrough | WorkloadClass::StructuredOutput
        )
    {
        return;
    }

    let is_array_system = ir.root.get("system").is_some_and(Value::is_array);
    if !ir.is_turn_zero && !is_array_system {
        return;
    }

    match ir.dialect {
        Dialect::OpenAiChat | Dialect::AnthropicMessages | Dialect::OllamaNative => {
            if let Some(sys) = ir
                .root
                .get("system")
                .and_then(Value::as_str)
                .map(str::to_owned)
            {
                if !sys.contains("[Output Policy:") {
                    ir.root["system"] = Value::String(format!("{sys}{CONCISE_OUTPUT_DIRECTIVE}"));
                }
            } else if let Some(blocks) = ir.root.get_mut("system").and_then(Value::as_array_mut) {
                let already_present = blocks.iter().any(|b| {
                    b.get("text")
                        .and_then(Value::as_str)
                        .is_some_and(|t| t.contains("[Output Policy:"))
                });
                if !already_present {
                    blocks.push(json!({
                        "type": "text",
                        "text": CONCISE_OUTPUT_DIRECTIVE.trim_start()
                    }));
                }
            } else if let Some(first_msg) = ir
                .root
                .get_mut("messages")
                .and_then(Value::as_array_mut)
                .and_then(|m| m.first_mut())
            {
                let role = first_msg.get("role").and_then(Value::as_str).unwrap_or_default();
                if matches!(role, "system" | "developer") {
                    if let Some(content) = first_msg
                        .get("content")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                    {
                        if !content.contains("[Output Policy:") {
                            first_msg["content"] =
                                Value::String(format!("{content}{CONCISE_OUTPUT_DIRECTIVE}"));
                        }
                    }
                }
            }
        }
        Dialect::OpenAiResponses => {
            if let Some(inst) = ir
                .root
                .get("instructions")
                .and_then(Value::as_str)
                .map(str::to_owned)
            {
                if !inst.contains("[Output Policy:") {
                    ir.root["instructions"] =
                        Value::String(format!("{inst}{CONCISE_OUTPUT_DIRECTIVE}"));
                }
            }
        }
        Dialect::GeminiGenerate => {
            if let Some(parts) = ir
                .root
                .pointer_mut("/systemInstruction/parts")
                .and_then(Value::as_array_mut)
            {
                if let Some(first) = parts.first_mut() {
                    if let Some(text) = first.get("text").and_then(Value::as_str).map(str::to_owned)
                    {
                        if !text.contains("[Output Policy:") {
                            first["text"] =
                                Value::String(format!("{text}{CONCISE_OUTPUT_DIRECTIVE}"));
                        }
                    }
                }
            }
        }
        Dialect::Passthrough => {}
    }
}

/// Maps optional `X-Aphelion-Reasoning-Effort: low | medium | high` (or opt-in
/// `APHELION_AUTO_REASONING=safe` on deterministic mechanical turns) to provider-native fields.
pub fn apply_reasoning_budget(ir: &mut NormalizedRequest, opts: &RequestOptions) {
    let effort_raw = match opts.reasoning_effort.as_deref() {
        Some(explicit) => explicit,
        None if opts.routing.auto_reasoning_safe
            && crate::tier_router::is_safe_low_reasoning_turn(ir) =>
        {
            "low"
        }
        None => return,
    };
    let effort = match effort_raw.trim().to_ascii_lowercase().as_str() {
        "low" => "low",
        "medium" => "medium",
        "high" => "high",
        _ => return,
    };

    let Some(obj) = ir.root.as_object_mut() else {
        return;
    };
    match ir.dialect {
        Dialect::AnthropicMessages => {
            let budget = match effort {
                "low" => 2_048,
                "medium" => 8_192,
                _ => 16_384,
            };
            obj.insert(
                "thinking".to_owned(),
                json!({ "type": "enabled", "budget_tokens": budget }),
            );
        }
        Dialect::OpenAiChat | Dialect::OpenAiResponses => {
            if opts.provider_id == "openrouter" {
                obj.insert("reasoning".to_owned(), json!({ "effort": effort }));
            } else {
                obj.insert(
                    "reasoning_effort".to_owned(),
                    Value::String(effort.to_owned()),
                );
            }
        }
        _ => {}
    }
}

/// Detects degenerate output repetition loops in an SSE ring buffer:
/// returns `true` if any trailing block of length `>= min_block` repeats `>= reps` consecutive times.
pub fn detect_consecutive_repeat(buf: &[u8], min_block: usize, reps: usize) -> bool {
    if min_block == 0 || reps < 2 || buf.len() < min_block * reps {
        return false;
    }
    let max_block = (buf.len() / reps).min(256);
    for block_len in min_block..=max_block {
        let start = buf.len() - block_len * reps;
        let first = &buf[start..start + block_len];
        let all_match =
            (1..reps).all(|i| &buf[start + i * block_len..start + (i + 1) * block_len] == first);
        if all_match {
            return true;
        }
    }
    false
}
