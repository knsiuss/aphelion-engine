use crate::diet_gc::estimate_calibrated_tokens;
use crate::ingress_anthropic::{
    build_tool_use_map, optimize_anthropic_tool_results_ingress, seed_frozen_file_hashes,
};
use crate::prefix_ledger::FrozenSessionRecord;
use crate::tron_codec::try_encode_tron_table;
use crate::types::{ExecutionProfile, NormalizedRequest, StageStats};
use serde_json::Value;
use xxhash_rust::xxh3::xxh3_64;

/// Pre-Commit Ingress Optimization ("Optimize Once, Freeze Forever"):
/// Operates EXCLUSIVELY on newly appended messages strictly after both `ingress_start` and
/// `last_cached_msg_idx` (`messages[ir.mutable_ingress_start()..]`). Never mutates frozen or
/// `cache_control` history `0..mutable_start`, never strips `<think>` / `thinking` blocks,
/// and never deletes unique/modified file reads.
pub fn optimize_ingress_messages(
    ir: &mut NormalizedRequest,
    record: &mut FrozenSessionRecord,
    _profile: ExecutionProfile,
) {
    if record.cache_drop_breaker_tripped {
        ir.stage_stats.cache_drop_breaker_active = true;
        return;
    }

    if let Some(items) = ir.root.get_mut("input").and_then(Value::as_array_mut) {
        let start = crate::prefix_ledger::replay_frozen_history(items, record);
        ir.ingress_start = start;
        for prev in &items[..start] {
            if prev.get("type").and_then(Value::as_str) == Some("function_call_output") {
                if let Some(raw) = prev.get("output").and_then(Value::as_str) {
                    if let Some(path) = extract_file_read_path("function_call_output", raw) {
                        record.frozen_file_hashes.insert(path, xxh3_64(raw.as_bytes()));
                    }
                }
            }
        }
        for item in &mut items[start..] {
            if item.get("type").and_then(Value::as_str) == Some("function_call_output") {
                if let Some(raw) = item
                    .get("output")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                {
                    if let Some((opt, saved, tron)) =
                        optimize_tool_payload("function_call_output", None, &raw, record)
                    {
                        item["output"] = Value::String(opt);
                        ir.stage_stats.ingress_saved_tokens += saved;
                        if tron {
                            ir.stage_stats.tron_tables_encoded += 1;
                        }
                    }
                }
            }
        }
    }

    let mutable_start = ir.mutable_ingress_start();
    let allow_cached_new_turn = !ir.is_turn_zero && ir.ingress_start > 0;
    let stats = &mut ir.stage_stats;
    let Some(messages) = ir.root.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    if mutable_start >= messages.len() {
        return;
    }

    let tool_map = build_tool_use_map(messages);
    seed_frozen_file_hashes(&messages[..mutable_start], &tool_map, record);

    for msg in &mut messages[mutable_start..] {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or_default();
        if role == "tool" {
            optimize_openai_tool_ingress(msg, record, stats);
        } else if role == "user" {
            optimize_anthropic_tool_results_ingress(
                msg,
                &tool_map,
                record,
                stats,
                allow_cached_new_turn,
            );
        }
    }
}

fn optimize_openai_tool_ingress(
    msg: &mut Value,
    record: &mut FrozenSessionRecord,
    stats: &mut StageStats,
) {
    let name = msg
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let Some(raw) = msg
        .get("content")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return;
    };
    let Some((optimized, saved, encoded_tron)) = optimize_tool_payload(&name, None, &raw, record)
    else {
        return;
    };
    msg["content"] = Value::String(optimized);
    stats.ingress_saved_tokens += saved;
    if encoded_tron {
        stats.tron_tables_encoded += 1;
    }
}

pub(crate) fn is_read_tool(tool_name: &str) -> bool {
    let lower = tool_name.to_ascii_lowercase();
    lower == "read" || lower.contains("read") || lower == "view_file"
}

fn is_log_tool(tool_name: &str) -> bool {
    let lower = tool_name.to_ascii_lowercase();
    lower == "bash"
        || lower.contains("shell")
        || lower.contains("exec")
        || lower.contains("test")
        || lower.contains("log")
        || lower.contains("terminal")
}

pub(crate) fn optimize_tool_payload(
    tool_name: &str,
    hint_path: Option<&str>,
    raw: &str,
    record: &mut FrozenSessionRecord,
) -> Option<(String, usize, bool)> {
    let before = estimate_calibrated_tokens(raw);

    let file_path = if is_read_tool(tool_name) {
        hint_path
            .map(str::to_owned)
            .or_else(|| extract_file_read_path(tool_name, raw))
    } else {
        extract_file_read_path(tool_name, raw)
    };

    if let Some(path) = file_path {
        let hash = xxh3_64(raw.as_bytes());
        if record.frozen_file_hashes.get(&path).copied() == Some(hash) && before > 40 {
            let dedup = format!(
                "[Aphelion Ingress Dedup: `{path}` unchanged since prior read in frozen prefix (xxh3:{:08x})]",
                hash as u32
            );
            let after = estimate_calibrated_tokens(&dedup);
            return Some((dedup, before.saturating_sub(after), false));
        }
        record.frozen_file_hashes.insert(path, hash);
        if is_read_tool(tool_name) {
            return None;
        }
    }

    if let Some(tron) = try_encode_tron_table(raw) {
        let after = estimate_calibrated_tokens(&tron);
        if after < before {
            return Some((tron, before - after, true));
        }
    }

    if is_log_tool(tool_name) || !is_source_code_payload(raw) {
        let deduped = collapse_repetitive_log_lines(raw);
        let after = estimate_calibrated_tokens(&deduped);
        if after < before {
            return Some((deduped, before - after, false));
        }
    }

    None
}

fn extract_file_read_path(tool_name: &str, raw: &str) -> Option<String> {
    if let Some(rest) = raw.strip_prefix("File:") {
        let path = rest.lines().next()?.trim();
        if !path.is_empty() {
            return Some(path.to_owned());
        }
    }
    if is_read_tool(tool_name) {
        let first = raw.lines().next()?.trim();
        if first.len() < 160 && (first.contains('/') || first.contains('.')) {
            return Some(first.to_owned());
        }
    }
    None
}

fn is_source_code_payload(raw: &str) -> bool {
    raw.contains("```")
        || raw.starts_with("File:")
        || raw.contains("pub fn ")
        || raw.contains("fn ")
        || raw.contains("def ")
        || raw.contains("export function ")
}

/// Collapses runs of identical consecutive log lines (`[... repeated N more times ...]`).
pub fn collapse_repetitive_log_lines(raw: &str) -> String {
    let mut out = Vec::new();
    let mut prev: Option<&str> = None;
    let mut run = 0usize;

    for line in raw.lines() {
        let trimmed_end = line.trim_end();
        if prev == Some(trimmed_end) {
            run += 1;
        } else {
            flush_run(&mut out, prev, run);
            out.push(trimmed_end.to_owned());
            prev = Some(trimmed_end);
            run = 0;
        }
    }
    flush_run(&mut out, prev, run);
    out.join("\n")
}

fn flush_run(out: &mut Vec<String>, prev: Option<&str>, run: usize) {
    if run > 1 {
        out.push(format!("  [... repeated {run} more times ...]"));
    } else if run == 1 {
        if let Some(p) = prev {
            out.push(p.to_owned());
        }
    }
}
