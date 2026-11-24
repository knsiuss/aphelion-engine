use crate::types::NormalizedRequest;
use serde_json::{Map, Value};

/// Transcodes ONLY inbound homogeneous JSON arrays (>= 3 objects with identical keys)
/// inside `tool_result` / `role == "tool"` payloads into tabular `@tron[k1,k2,...]`.
/// Hard Invariant 5: outbound `tools[]` and `tool_calls` remain 100% standard JSON.
pub fn transcode_inbound_tool_results(ir: &mut NormalizedRequest) {
    let Some(messages) = ir.root.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    let Some(last) = messages.last_mut() else {
        return;
    };

    if last.get("role").and_then(Value::as_str) == Some("tool") {
        if let Some(raw_content) = last.get("content").and_then(Value::as_str) {
            if let Some(tron) = try_encode_tron_table(raw_content) {
                last["content"] = Value::String(tron);
                ir.stage_stats.tron_tables_encoded += 1;
            }
        }
        return;
    }

    // Anthropic `tool_result` content blocks inside a `user` message.
    if let Some(blocks) = last.get_mut("content").and_then(Value::as_array_mut) {
        for block in blocks {
            if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                if let Some(raw_content) = block.get("content").and_then(Value::as_str) {
                    if let Some(tron) = try_encode_tron_table(raw_content) {
                        block["content"] = Value::String(tron);
                        ir.stage_stats.tron_tables_encoded += 1;
                    }
                }
            }
        }
    }
}

pub fn try_encode_tron_table(json_str: &str) -> Option<String> {
    let Value::Array(items) = serde_json::from_str::<Value>(json_str).ok()? else {
        return None;
    };
    if items.len() < 3 {
        return None;
    }
    let first_obj = items.first()?.as_object()?;
    if first_obj.is_empty() {
        return None;
    }
    let keys: Vec<&String> = first_obj.keys().collect();
    let mut rows = Vec::with_capacity(items.len() + 1);
    let header_cols: Vec<&str> = keys.iter().map(|k| k.as_str()).collect();
    rows.push(format!("@tron[{}]", header_cols.join(",")));

    for item in &items {
        let obj = item.as_object()?;
        if obj.len() != keys.len() {
            return None;
        }
        let mut cells = Vec::with_capacity(keys.len());
        for k in &keys {
            let val = obj.get(*k)?;
            if val.is_object() || val.is_array() {
                return None;
            }
            let cell = match val {
                Value::String(s) => s.replace('|', "\\|").replace('\n', "\\n"),
                other => other.to_string(),
            };
            cells.push(cell);
        }
        rows.push(cells.join("|"));
    }

    Some(rows.join("\n"))
}

/// Deterministic decoder for verifying lossless round-trip of `@tron[...]` tables.
pub fn decode_tron_table(tron: &str) -> Option<Value> {
    let mut lines = tron.lines();
    let header = lines.next()?;
    let cols_raw = header.strip_prefix("@tron[")?.strip_suffix(']')?;
    let keys: Vec<&str> = cols_raw.split(',').collect();
    if keys.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for line in lines {
        let cells = split_escaped_pipe(line);
        if cells.len() != keys.len() {
            return None;
        }
        let mut map = Map::new();
        for (k, cell) in keys.iter().zip(cells) {
            let val = parse_tron_cell(&cell);
            map.insert((*k).to_owned(), val);
        }
        out.push(Value::Object(map));
    }
    Some(Value::Array(out))
}

fn split_escaped_pipe(line: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut escaped = false;
    for ch in line.chars() {
        if escaped {
            match ch {
                'n' => cur.push('\n'),
                other => cur.push(other),
            }
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '|' {
            cells.push(std::mem::take(&mut cur));
        } else {
            cur.push(ch);
        }
    }
    cells.push(cur);
    cells
}

fn parse_tron_cell(cell: &str) -> Value {
    if cell == "null" {
        Value::Null
    } else if cell == "true" {
        Value::Bool(true)
    } else if cell == "false" {
        Value::Bool(false)
    } else if let Ok(i) = cell.parse::<i64>() {
        Value::Number(i.into())
    } else if let Ok(f) = cell.parse::<f64>() {
        serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or_else(|| Value::String(cell.to_owned()))
    } else {
        Value::String(cell.to_owned())
    }
}
