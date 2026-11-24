use crate::prefix_ledger::should_run_macro_compaction;
use crate::types::{NormalizedRequest, SessionState};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// Cache amortization watermark calibrated in BPE tokens (never raw `bytes / 4`).
pub const CACHE_AMORTIZATION_WATERMARK_TOKENS: usize = 4_000;

/// Symbol-aware BPE token estimator calibrated to `cl100k_base` / `o200k_base`
/// for code, JSON punctuation, indentation runs, and natural prose.
pub fn estimate_calibrated_tokens(text: &str) -> usize {
    let mut tokens = 0usize;
    let mut word_len = 0usize;
    let mut ws_run = 0usize;

    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            if ws_run > 0 {
                tokens += ws_run.div_ceil(4);
                ws_run = 0;
            }
            word_len += 1;
            if word_len == 4 {
                tokens += 1;
                word_len = 0;
            }
        } else if ch.is_ascii_whitespace() {
            if word_len > 0 {
                tokens += 1;
                word_len = 0;
            }
            if ch == '\n' {
                if ws_run > 0 {
                    tokens += ws_run.div_ceil(4);
                    ws_run = 0;
                }
                tokens += 1;
            } else {
                ws_run += 1;
            }
        } else {
            if word_len > 0 {
                tokens += 1;
                word_len = 0;
            }
            if ws_run > 0 {
                tokens += ws_run.div_ceil(4);
                ws_run = 0;
            }
            tokens += if ch.is_ascii() { 1 } else { 2 };
        }
    }
    if word_len > 0 {
        tokens += 1;
    }
    if ws_run > 0 {
        tokens += ws_run.div_ceil(4);
    }
    tokens
}

/// Amortized Macro-Compaction Only: disables per-turn micro-GC on historical turns so
/// upstream KV-caches are never invalidated mid-trajectory. Only runs a compaction checkpoint
/// when total context >= 120,000 BPE tokens AND `re_prefill_tokens * 10 < projected_saved_reads`.
pub fn compact_if_amortized(ir: &mut NormalizedRequest, state: &SessionState) {
    if ir.is_turn_zero || state.sacred_cache_boundary > 0 {
        return;
    }
    let key = state.ledger_key(&ir.model_id);
    let breaker_tripped = state
        .prefix_ledger
        .get(&key)
        .map(|r| r.cache_drop_breaker_tripped)
        .unwrap_or(false);

    let Some(messages) = ir.root.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    if messages.len() < 4 {
        return;
    }

    let last_idx = messages.len() - 1;
    let later_edits = collect_edited_paths(messages);
    let tests_passed = has_later_passing_test(messages);
    let pinned_indices = collect_pinned_read_indices(messages);

    let mut stale_tokens = 0usize;
    let mut candidates: Vec<(usize, &'static str, usize)> = Vec::new();

    for (idx, msg) in messages[..last_idx].iter().enumerate() {
        if pinned_indices.contains(&idx) {
            continue;
        }
        if msg.get("role").and_then(Value::as_str) != Some("tool") {
            continue;
        }
        let Some(content) = msg.get("content").and_then(Value::as_str) else {
            continue;
        };
        if content.starts_with("[AgentDiet Tombstone:") {
            continue;
        }
        let tok = estimate_calibrated_tokens(content);
        if tok <= 250 {
            continue;
        }
        if let Some(kind) = classify_stale_reason(msg, content, &later_edits, tests_passed) {
            stale_tokens += tok;
            candidates.push((idx, kind, content.lines().count()));
        }
    }

    if stale_tokens >= CACHE_AMORTIZATION_WATERMARK_TOKENS
        && should_run_macro_compaction(messages, stale_tokens, 12, breaker_tripped)
    {
        ir.stage_stats.diet_gc_compacted_tokens = stale_tokens;
        ir.stage_stats.macro_compaction_triggered = true;
        for (idx, kind, lines) in candidates {
            messages[idx]["content"] = Value::String(format!(
                "[AgentDiet Tombstone: {kind} ({lines} lines) compacted at 120k checkpoint; saved {stale_tokens} BPE tok]"
            ));
        }
    }
}

/// Active-Reference Pinning: finds file paths referenced in the final 2 turns and pins
/// the most recent `read_file` observation for each active file so context is never starved.
fn collect_pinned_read_indices(messages: &[Value]) -> HashSet<usize> {
    let mut active_refs = HashSet::new();
    for msg in messages.iter().rev().take(2) {
        let serialized = msg.to_string();
        for tok in serialized.split(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    '"' | '\'' | ',' | ';' | '(' | ')' | '{' | '}' | '[' | ']'
                )
        }) {
            if is_file_path_like(tok) {
                active_refs.insert(tok.to_owned());
            }
        }
    }
    if active_refs.is_empty() {
        return HashSet::new();
    }

    let mut latest_read_by_path: HashMap<String, usize> = HashMap::new();
    for (idx, msg) in messages.iter().enumerate() {
        if msg.get("role").and_then(Value::as_str) != Some("tool") {
            continue;
        }
        let name = msg.get("name").and_then(Value::as_str).unwrap_or_default();
        let content = msg
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !name.contains("read") && !content.starts_with("File:") {
            continue;
        }
        for path in &active_refs {
            if content.contains(path.as_str()) {
                latest_read_by_path.insert(path.clone(), idx);
            }
        }
    }
    latest_read_by_path.into_values().collect()
}

fn is_file_path_like(tok: &str) -> bool {
    if tok.len() < 4 || tok.len() > 160 {
        return false;
    }
    tok.contains('/')
        || tok.ends_with(".rs")
        || tok.ends_with(".ts")
        || tok.ends_with(".js")
        || tok.ends_with(".py")
        || tok.ends_with(".toml")
        || tok.ends_with(".json")
}

fn classify_stale_reason(
    msg: &Value,
    content: &str,
    edited_paths: &HashSet<String>,
    tests_passed: bool,
) -> Option<&'static str> {
    let name = msg.get("name").and_then(Value::as_str).unwrap_or_default();
    if (name.contains("read") || content.starts_with("File:"))
        && (edited_paths.iter().any(|p| content.contains(p.as_str())) || edited_paths.is_empty())
    {
        return Some("superseded file read");
    }
    if tests_passed
        && (content.contains("Error:")
            || content.contains("Traceback")
            || content.contains("FAILED"))
    {
        return Some("resolved error trace");
    }
    if name.contains("grep") || name.contains("search") || name.contains("find") {
        return Some("unreferenced search result");
    }
    Some("archived tool observation")
}

fn collect_edited_paths(messages: &[Value]) -> HashSet<String> {
    let mut paths = HashSet::new();
    for msg in messages {
        if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let fn_name = call
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if fn_name.contains("edit") || fn_name.contains("write") {
                    if let Some(args) = call
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(Value::as_str)
                    {
                        if let Ok(v) = serde_json::from_str::<Value>(args) {
                            if let Some(p) = v.get("path").and_then(Value::as_str) {
                                paths.insert(p.to_owned());
                            }
                        }
                    }
                }
            }
        }
    }
    paths
}

fn has_later_passing_test(messages: &[Value]) -> bool {
    messages.iter().rev().take(3).any(|m| {
        m.get("content")
            .and_then(Value::as_str)
            .map(|c| {
                c.contains("test result: ok") || c.contains("PASSED") || c.contains("0 failed")
            })
            .unwrap_or(false)
    })
}
