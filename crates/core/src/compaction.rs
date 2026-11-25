use crate::defaults::{
    DEFAULT_COMPACTION_THRESHOLD_TOKENS, DEFAULT_LOCKED_TAIL_MESSAGES,
    DEFAULT_PROJECTED_REMAINING_TURNS,
};
use crate::diet_gc::estimate_calibrated_tokens;
use crate::prefix_ledger::{commit_outbound_messages, FrozenSessionRecord};
use crate::types::{CompactMode, NormalizedRequest, SessionState};
use serde_json::{json, Value};

/// Amortized Break-Even Gate for Macro-Compaction:
/// Strictly NO per-turn micro-GC. Triggers ONLY when `mode == Force` OR (`mode == Auto` AND
/// `total_tokens >= threshold` AND `post_compact_tokens * 1.0 < tokens_reclaimed * 0.10 * turns`).
pub fn should_compact(
    messages: &[Value],
    mode: CompactMode,
    threshold_tokens: usize,
    breaker_tripped: bool,
) -> Option<(usize, usize)> {
    should_compact_with_tail(
        messages,
        mode,
        threshold_tokens,
        DEFAULT_LOCKED_TAIL_MESSAGES,
        breaker_tripped,
    )
}

pub fn should_compact_with_tail(
    messages: &[Value],
    mode: CompactMode,
    threshold_tokens: usize,
    tail_keep: usize,
    breaker_tripped: bool,
) -> Option<(usize, usize)> {
    let keep = tail_keep.max(2);
    if mode == CompactMode::Off || messages.len() <= keep + 2 {
        return None;
    }
    if breaker_tripped && mode != CompactMode::Force {
        return None;
    }

    let tail_start = messages.len() - keep;
    let total_tokens: usize = messages
        .iter()
        .map(|m| estimate_calibrated_tokens(&m.to_string()))
        .sum();
    let middle_tokens: usize = messages[1..tail_start]
        .iter()
        .map(|m| estimate_calibrated_tokens(&m.to_string()))
        .sum();
    let checkpoint_est = 120usize;
    let tokens_reclaimed = middle_tokens.saturating_sub(checkpoint_est);
    let post_compact_tokens = total_tokens.saturating_sub(tokens_reclaimed);

    if mode == CompactMode::Force {
        return Some((total_tokens, tokens_reclaimed));
    }

    if total_tokens < threshold_tokens || tokens_reclaimed == 0 {
        return None;
    }

    // Amortized cost inequality: post_compact * 1.0 < reclaimed * 0.10 * projected_remaining_turns
    let re_prefill_cost = post_compact_tokens as f64;
    let projected_savings =
        (tokens_reclaimed as f64) * 0.10 * (DEFAULT_PROJECTED_REMAINING_TURNS as f64);
    if re_prefill_cost < projected_savings {
        Some((total_tokens, tokens_reclaimed))
    } else {
        None
    }
}

/// Root- & Tail-Preserving Structured Checkpoint with Atomic `PrefixLedger` Epoch Rollover:
/// Locks Turn 0 (`messages[0]`, System, `tools[]`) and latest `K` tail messages verbatim,
/// collapsing middle `1..tail_start` into a deterministic `[APHELION_CHECKPOINT]` message.
pub fn run_amortized_compaction(ir: &mut NormalizedRequest, state: &mut SessionState) {
    if ir.is_turn_zero {
        return;
    }
    let key = state.ledger_key(&ir.model_id);
    let breaker_tripped = state
        .prefix_ledger
        .get(&key)
        .map(|r| r.cache_drop_breaker_tripped)
        .unwrap_or(false);
    let mode = state.request_options.compact_mode;
    let threshold = state
        .request_options
        .compaction_threshold_tokens
        .unwrap_or(DEFAULT_COMPACTION_THRESHOLD_TOKENS);
    let tail_keep = state
        .request_options
        .locked_tail_messages
        .unwrap_or(DEFAULT_LOCKED_TAIL_MESSAGES)
        .max(2);

    let Some(messages) = ir.root.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    let Some((_total, reclaimed)) =
        should_compact_with_tail(messages, mode, threshold, tail_keep, breaker_tripped)
    else {
        return;
    };

    let mut tail_start = messages.len() - tail_keep;
    // Align tail_start backward so we never orphan a `role: "tool"` / `tool_result` from its assistant call.
    while tail_start > 1 && is_tool_response(&messages[tail_start]) {
        tail_start -= 1;
    }
    if tail_start <= 1 {
        return;
    }
    let record_snapshot = state.prefix_ledger.get(&key).cloned().unwrap_or_default();
    let checkpoint_text = build_checkpoint_summary(&messages[1..tail_start], &record_snapshot);

    let mut compacted = Vec::with_capacity(messages.len() - tail_start + 2);
    compacted.push(messages[0].clone());
    compacted.push(json!({
        "role": "user",
        "content": checkpoint_text
    }));
    compacted.extend_from_slice(&messages[tail_start..]);
    *messages = compacted;

    // Atomic PrefixLedger Epoch Rollover: immediately restores S_prefix == 1.00 on next turn.
    let rec = state.prefix_ledger.get_or_insert_mut(key);
    rec.compaction_epoch += 1;
    commit_outbound_messages(messages, rec);
    ir.ingress_start = messages.len();
    ir.last_cached_msg_idx = messages.iter().rposition(crate::normalizer::has_cache_control);
    state.last_cached_msg_idx = ir.last_cached_msg_idx;
    ir.stage_stats.macro_compaction_triggered = true;
    ir.stage_stats.compaction_tokens_reclaimed = reclaimed;
    ir.stage_stats.diet_gc_compacted_tokens = reclaimed;
}

fn build_checkpoint_summary(middle: &[Value], record: &FrozenSessionRecord) -> String {
    let mut files: Vec<String> = record
        .frozen_file_hashes
        .iter()
        .map(|(p, h)| format!("{p}# {:08x}", *h as u32))
        .collect();
    files.sort();

    let mut unresolved_errors = Vec::new();
    for msg in middle.iter().rev().take(6) {
        if let Some(c) = msg.get("content").and_then(Value::as_str) {
            if c.contains("Error:") || c.contains("panic:") || c.contains("FAILED") {
                let first_line = c.lines().next().unwrap_or_default();
                unresolved_errors.push(first_line.chars().take(120).collect::<String>());
            }
        }
    }

    let files_str = if files.is_empty() {
        "none".to_owned()
    } else {
        files.join(", ")
    };
    let errors_str = if unresolved_errors.is_empty() {
        "none".to_owned()
    } else {
        unresolved_errors.join(" | ")
    };

    format!(
        "[APHELION_CHECKPOINT epoch={}]\nCompacted {} historical messages.\nActive file hashes: {}\nUnresolved errors: {}",
        record.compaction_epoch + 1,
        middle.len(),
        files_str,
        errors_str
    )
}

fn is_tool_response(msg: &Value) -> bool {
    if msg.get("role").and_then(Value::as_str) == Some("tool") {
        return true;
    }
    msg.get("content")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
        })
        .unwrap_or(false)
}
