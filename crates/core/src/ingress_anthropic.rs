use crate::ingress_opt::{is_read_tool, optimize_tool_payload};
use crate::prefix_ledger::FrozenSessionRecord;
use crate::types::StageStats;
use serde_json::Value;
use std::collections::HashMap;
use xxhash_rust::xxh3::xxh3_64;

#[derive(Debug, Clone)]
pub struct ToolUseMeta {
    pub name: String,
    pub hint_path: Option<String>,
}

/// Returns true if a JSON subtree contains a non-null `"cache_control"` marker.
pub fn has_cache_control(val: &Value) -> bool {
    match val {
        Value::Object(map) => {
            if map.get("cache_control").is_some_and(|v| !v.is_null()) {
                return true;
            }
            map.values().any(has_cache_control)
        }
        Value::Array(arr) => arr.iter().any(has_cache_control),
        _ => false,
    }
}

/// Returns the highest index `i` in `messages[]` containing any `cache_control` block.
pub fn find_last_cached_msg_idx(root: &Value) -> Option<usize> {
    root.get("messages")
        .and_then(Value::as_array)?
        .iter()
        .rposition(has_cache_control)
}

/// Computes the byte offset immediately after the final `"cache_control": { ... }` object
/// so Turn-0 prefix freezing ends strictly before any newly appended array elements.
pub fn find_last_cache_control_end(raw_json: &[u8]) -> usize {
    let needle = b"\"cache_control\"";
    let Some(pos) = raw_json.windows(needle.len()).rposition(|w| w == needle) else {
        return 0;
    };
    let mut i = pos + needle.len();
    let mut depth = 0usize;
    let mut saw_obj = false;
    while i < raw_json.len() {
        match raw_json[i] {
            b'{' => {
                depth += 1;
                saw_obj = true;
            }
            b'}' => {
                if depth > 0 {
                    depth -= 1;
                    if saw_obj && depth == 0 {
                        return i + 1;
                    }
                } else {
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    pos.saturating_add(needle.len())
}

/// Scans all assistant `tool_use` blocks to map `tool_use_id -> (tool_name, hint_path)`.
pub fn build_tool_use_map(messages: &[Value]) -> HashMap<String, ToolUseMeta> {
    let mut map = HashMap::new();
    for msg in messages {
        let Some(blocks) = msg.get("content").and_then(Value::as_array) else {
            continue;
        };
        for block in blocks {
            if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            let Some(id) = block.get("id").and_then(Value::as_str) else {
                continue;
            };
            let name = block
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let hint_path = block.get("input").and_then(|inp| {
                inp.get("file_path")
                    .or_else(|| inp.get("path"))
                    .or_else(|| inp.get("command"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
            map.insert(id.to_owned(), ToolUseMeta { name, hint_path });
        }
    }
    map
}

/// Read-only pass over frozen/cached history `messages[..start]` so repeated `Read` tool calls
/// strictly after `last_cached_msg_idx` deduplicate accurately without mutating cached bytes.
pub fn seed_frozen_file_hashes(
    frozen_slice: &[Value],
    tool_map: &HashMap<String, ToolUseMeta>,
    record: &mut FrozenSessionRecord,
) {
    for msg in frozen_slice {
        let Some(blocks) = msg.get("content").and_then(Value::as_array) else {
            continue;
        };
        for block in blocks {
            if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            let meta = block
                .get("tool_use_id")
                .and_then(Value::as_str)
                .and_then(|id| tool_map.get(id));
            let Some(meta) = meta else { continue };
            if !is_read_tool(&meta.name) {
                continue;
            }
            let Some(path) = meta.hint_path.as_deref() else {
                continue;
            };
            if let Some(raw) = block.get("content").and_then(Value::as_str) {
                record
                    .frozen_file_hashes
                    .insert(path.to_owned(), xxh3_64(raw.as_bytes()));
            } else if let Some(sub_blocks) = block.get("content").and_then(Value::as_array) {
                for sb in sub_blocks {
                    if sb.get("type").and_then(Value::as_str) == Some("text") {
                        if let Some(text) = sb.get("text").and_then(Value::as_str) {
                            record
                                .frozen_file_hashes
                                .insert(path.to_owned(), xxh3_64(text.as_bytes()));
                        }
                    }
                }
            }
        }
    }
}

/// Optimizes Anthropic `tool_result` blocks (both `String` and content-block `Array`) using
/// resolved `tool_use_id` metadata. On Turn-0 (`!allow_cached_new_turn`), skips any block
/// marked with `cache_control`; on Turn-N (`allow_cached_new_turn`), optimizes newly appended
/// undispatched blocks at `>= ingress_start` while preserving their `cache_control` marker.
pub fn optimize_anthropic_tool_results_ingress(
    msg: &mut Value,
    tool_map: &HashMap<String, ToolUseMeta>,
    record: &mut FrozenSessionRecord,
    stats: &mut StageStats,
    allow_cached_new_turn: bool,
) {
    let Some(blocks) = msg.get_mut("content").and_then(Value::as_array_mut) else {
        return;
    };
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("tool_result")
            || (!allow_cached_new_turn && has_cache_control(block))
        {
            continue;
        }
        let meta = block
            .get("tool_use_id")
            .and_then(Value::as_str)
            .and_then(|id| tool_map.get(id));
        let tool_name = meta.map(|m| m.name.as_str()).unwrap_or("tool_result");
        let hint_path = meta.and_then(|m| m.hint_path.as_deref());

        if let Some(raw) = block
            .get("content")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            if let Some((opt, saved, tron)) =
                optimize_tool_payload(tool_name, hint_path, &raw, record)
            {
                block["content"] = Value::String(opt);
                stats.ingress_saved_tokens += saved;
                if tron {
                    stats.tron_tables_encoded += 1;
                }
            }
        } else if let Some(sub_blocks) = block.get_mut("content").and_then(Value::as_array_mut) {
            for sb in sub_blocks {
                if sb.get("type").and_then(Value::as_str) != Some("text")
                    || (!allow_cached_new_turn && has_cache_control(sb))
                {
                    continue;
                }
                let Some(text) = sb.get("text").and_then(Value::as_str).map(str::to_owned) else {
                    continue;
                };
                if let Some((opt, saved, tron)) =
                    optimize_tool_payload(tool_name, hint_path, &text, record)
                {
                    sb["text"] = Value::String(opt);
                    stats.ingress_saved_tokens += saved;
                    if tron {
                        stats.tron_tables_encoded += 1;
                    }
                }
            }
        }
    }
}
