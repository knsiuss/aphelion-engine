use crate::types::{
    Complexity, Dialect, NormalizedRequest, PipelineFlags, SessionState, TransformMetrics,
    TransformedPayload,
};
use crate::{validator, CoreError};
use serde_json::{json, Value};

/// Enforces the Safe-Mode Routing Matrix, RoPE tail-steer directives, capability-aware prefill,
/// and final Zero-HTTP-400 message-graph validation with automatic rollback.
pub fn compose_outbound(
    mut ir: NormalizedRequest,
    state: &mut SessionState,
    flags: PipelineFlags,
) -> Result<TransformedPayload, CoreError> {
    if let Some(anchor) = &state.last_thought_anchor {
        if flags.ast_block_priming || flags.economizer_gates {
            ir.tail_directives
                .push(format!("[Prior Reasoning Anchor: {anchor}]"));
        }
    }

    if !ir.tail_directives.is_empty() {
        let suffix = format!("\n\n{}", ir.tail_directives.join("\n"));
        append_to_final_message(&mut ir.root, &suffix);
    }

    // Hard Invariant 2 & 6: Suppress prefill during active tool loops or DEEP complexity.
    let can_prime = flags.ast_block_priming
        && ir.complexity == Complexity::Trivial
        && !ir.active_tool_loop
        && ir.supports_prefill
        && ir.dialect == Dialect::OpenAiChat;

    if can_prime {
        let prefill = "<think>Target unambiguous; single execution path verified.</think>\n";
        ir.assistant_prefill = Some(prefill.to_owned());
        if let Some(messages) = ir.root.get_mut("messages").and_then(Value::as_array_mut) {
            messages.push(json!({ "role": "assistant", "content": prefill }));
        }
    }

    // Zero-HTTP-400 Guard: validate outbound message graph; roll back on any contract violation.
    if validator::validate_outbound_messages(ir.dialect, &ir.root).is_err() {
        if let Some(original_msgs) = ir.pre_compaction_messages.take() {
            ir.root["messages"] = original_msgs;
            ir.stage_stats.validator_rollback = true;
            ir.assistant_prefill = None;
        }
    }

    state.turn_index += 1;
    let body = serde_json::to_vec(&ir.root)?;
    let metrics = TransformMetrics {
        session_id_hex: format!("{:032x}", state.session_id.unwrap_or(0)),
        turn_alias: state.turn_alias,
        frozen_prefix_len: state.frozen_prefix_len,
        complexity_deep: ir.complexity == Complexity::Deep,
        active_tool_loop: ir.active_tool_loop,
        prefill_injected: ir.assistant_prefill.is_some(),
        slm_route: ir.slm_route,
        stage_stats: ir.stage_stats,
    };

    Ok(TransformedPayload { body, metrics })
}

fn append_to_final_message(root: &mut Value, suffix: &str) {
    let Some(messages) = root.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    let Some(last) = messages.last_mut() else {
        return;
    };
    if let Some(existing) = last.get("content").and_then(Value::as_str).map(str::to_owned) {
        last["content"] = Value::String(format!("{existing}{suffix}"));
    } else if let Some(blocks) = last.get_mut("content").and_then(Value::as_array_mut) {
        blocks.push(json!({ "type": "text", "text": suffix.trim_start() }));
    }
}
