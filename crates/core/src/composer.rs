use crate::prefix_ledger::commit_outbound_messages;
use crate::types::{
    Complexity, Dialect, NormalizedRequest, PipelineFlags, SessionState, TransformMetrics,
    TransformedPayload, WorkloadClass,
};
use crate::{validator, CoreError};
use serde_json::{json, Value};

/// Enforces WorkloadClass safety gates, append-only tail-steer directives (only on new ingress
/// messages `>= ingress_start`), Zero-HTTP-400 validation, and commits frozen prefix to `PrefixLedger`.
pub fn compose_outbound(
    mut ir: NormalizedRequest,
    state: &mut SessionState,
    flags: PipelineFlags,
) -> Result<TransformedPayload, CoreError> {
    if ir.workload == WorkloadClass::Passthrough {
        let body = ir.raw_passthrough.take().unwrap_or_default();
        return Ok(TransformedPayload {
            body,
            metrics: build_metrics(&ir, state),
        });
    }

    let msg_len = ir
        .root
        .get("messages")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let has_new_ingress = msg_len > ir.ingress_start;

    let allow_tail_directives = !ir.stage_stats.cache_drop_breaker_active
        && has_new_ingress
        && matches!(
            ir.workload,
            WorkloadClass::AgenticLoop | WorkloadClass::DirectCodeEdit
        );

    if allow_tail_directives {
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
    }

    let can_prime = !ir.stage_stats.cache_drop_breaker_active
        && has_new_ingress
        && flags.ast_block_priming
        && ir.workload == WorkloadClass::DirectCodeEdit
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

    if matches!(ir.dialect, Dialect::OpenAiChat | Dialect::AnthropicMessages)
        && validator::validate_outbound_messages(ir.dialect, &ir.root).is_err()
    {
        if let Some(original_msgs) = ir.pre_compaction_messages.take() {
            ir.root["messages"] = original_msgs;
            ir.stage_stats.validator_rollback = true;
            ir.assistant_prefill = None;
        }
    }

    // Commit canonical outbound messages into PrefixLedger so next turn replays `0..n` verbatim.
    if flags.prefix_lock {
        if let Some(messages) = ir.root.get("messages").and_then(Value::as_array) {
            let key = state.ledger_key(&ir.model_id);
            let rec = state.prefix_ledger.get_or_insert_mut(key);
            commit_outbound_messages(messages, rec);
        }
    }

    state.turn_index += 1;
    let body = serde_json::to_vec(&ir.root)?;
    let metrics = build_metrics(&ir, state);
    Ok(TransformedPayload { body, metrics })
}

fn build_metrics(ir: &NormalizedRequest, state: &SessionState) -> TransformMetrics {
    TransformMetrics {
        session_id_hex: format!("{:032x}", state.session_id.unwrap_or(0)),
        turn_alias: state.turn_alias,
        dialect: ir.dialect,
        workload: ir.workload,
        frozen_prefix_len: state.frozen_prefix_len,
        complexity_deep: ir.complexity == Complexity::Deep,
        active_tool_loop: ir.active_tool_loop,
        prefill_injected: ir.assistant_prefill.is_some(),
        slm_route: ir.slm_route.clone(),
        stage_stats: ir.stage_stats.clone(),
    }
}

fn append_to_final_message(root: &mut Value, suffix: &str) {
    let Some(messages) = root.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    let Some(last) = messages.last_mut() else {
        return;
    };
    if let Some(existing) = last
        .get("content")
        .and_then(Value::as_str)
        .map(str::to_owned)
    {
        last["content"] = Value::String(format!("{existing}{suffix}"));
    } else if let Some(blocks) = last.get_mut("content").and_then(Value::as_array_mut) {
        blocks.push(json!({ "type": "text", "text": suffix.trim_start() }));
    }
}
