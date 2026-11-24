use crate::diet_gc::estimate_calibrated_tokens;
use crate::prefix_ledger::FrozenSessionRecord;
use crate::tron_codec::try_encode_tron_table;
use crate::types::{ExecutionProfile, NormalizedRequest, StageStats};
use serde_json::Value;
use xxhash_rust::xxh3::xxh3_64;

/// Pre-Commit Ingress Optimization ("Optimize Once, Freeze Forever"):
/// Operates EXCLUSIVELY on newly appended messages `messages[ir.ingress_start..]` BEFORE their
/// very first upstream dispatch. Never mutates frozen history `0..ir.ingress_start`, never strips
/// `<think>` / `thinking` blocks, and never deletes unique/modified file reads.
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
        for item in items {
            if item.get("type").and_then(Value::as_str) == Some("function_call_output") {
                if let Some(raw) = item
                    .get("output")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                {
                    if let Some((opt, saved, tron)) =
                        optimize_tool_payload("function_call_output", &raw, record)
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

    let ingress_start = ir.ingress_start;
    let stats = &mut ir.stage_stats;
    let Some(messages) = ir.root.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    if ingress_start >= messages.len() {
        return;
    }

    for msg in &mut messages[ingress_start..] {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or_default();
        if role == "tool" {
            optimize_openai_tool_ingress(msg, record, stats);
        } else if role == "user" {
            optimize_anthropic_tool_results_ingress(msg, record, stats);
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
    let Some((optimized, saved, encoded_tron)) = optimize_tool_payload(&name, &raw, record) else {
        return;
    };
    msg["content"] = Value::String(optimized);
    stats.ingress_saved_tokens += saved;
    if encoded_tron {
        stats.tron_tables_encoded += 1;
    }
}

fn optimize_anthropic_tool_results_ingress(
    msg: &mut Value,
    record: &mut FrozenSessionRecord,
    stats: &mut StageStats,
) {
    let Some(blocks) = msg.get_mut("content").and_then(Value::as_array_mut) else {
        return;
    };
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        let Some(raw) = block
            .get("content")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            continue;
        };
        if let Some((optimized, saved, encoded_tron)) =
            optimize_tool_payload("tool_result", &raw, record)
        {
            block["content"] = Value::String(optimized);
            stats.ingress_saved_tokens += saved;
            if encoded_tron {
                stats.tron_tables_encoded += 1;
            }
        }
    }
}

fn optimize_tool_payload(
    tool_name: &str,
    raw: &str,
    record: &mut FrozenSessionRecord,
) -> Option<(String, usize, bool)> {
    let before = estimate_calibrated_tokens(raw);

    if let Some(path) = extract_file_read_path(tool_name, raw) {
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
    }

    if let Some(tron) = try_encode_tron_table(raw) {
        let after = estimate_calibrated_tokens(&tron);
        if after < before {
            return Some((tron, before - after, true));
        }
    }

    if !is_source_code_payload(raw) {
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
    if tool_name.contains("read") {
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
