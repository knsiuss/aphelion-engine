use crate::CoreError;
use serde_json::{json, Map, Value};

/// Pure Sans-I/O translator: Anthropic `/v1/messages` JSON -> OpenAI `/v1/chat/completions` JSON.
/// Preserves system prompts, tools[], tool_use/tool_result graphs, and streaming usage flags.
pub fn translate_anthropic_to_openai_request(raw_json: &[u8]) -> Result<Vec<u8>, CoreError> {
    let root: Value = serde_json::from_slice(raw_json)?;
    let Some(obj) = root.as_object() else {
        return Err(CoreError::UnsupportedDialect);
    };

    let mut out = Map::new();
    if let Some(m) = obj.get("model") {
        out.insert("model".to_owned(), m.clone());
    }
    if let Some(mt) = obj.get("max_tokens") {
        out.insert("max_tokens".to_owned(), mt.clone());
    }
    for key in ["temperature", "top_p", "stop_sequences"] {
        if let Some(v) = obj.get(key) {
            let dst = if key == "stop_sequences" { "stop" } else { key };
            out.insert(dst.to_owned(), v.clone());
        }
    }
    let streaming = obj.get("stream").and_then(Value::as_bool).unwrap_or(false);
    if streaming {
        out.insert("stream".to_owned(), Value::Bool(true));
        out.insert("stream_options".to_owned(), json!({ "include_usage": true }));
    }

    let mut oai_msgs: Vec<Value> = Vec::new();
    if let Some(sys) = obj.get("system") {
        let sys_text = flatten_text_value(sys);
        if !sys_text.is_empty() {
            oai_msgs.push(json!({ "role": "system", "content": sys_text }));
        }
    }

    if let Some(msgs) = obj.get("messages").and_then(Value::as_array) {
        for msg in msgs {
            translate_anthropic_msg(msg, &mut oai_msgs);
        }
    }
    out.insert("messages".to_owned(), Value::Array(oai_msgs));

    if let Some(tools) = obj.get("tools").and_then(Value::as_array) {
        let oai_tools: Vec<Value> = tools
            .iter()
            .filter_map(|t| {
                let name = t.get("name").and_then(Value::as_str)?;
                let mut func = Map::new();
                func.insert("name".to_owned(), Value::String(name.to_owned()));
                if let Some(desc) = t.get("description") {
                    func.insert("description".to_owned(), desc.clone());
                }
                if let Some(schema) = t.get("input_schema") {
                    func.insert("parameters".to_owned(), schema.clone());
                }
                Some(json!({ "type": "function", "function": Value::Object(func) }))
            })
            .collect();
        if !oai_tools.is_empty() {
            out.insert("tools".to_owned(), Value::Array(oai_tools));
        }
    }

    Ok(serde_json::to_vec(&Value::Object(out))?)
}

fn translate_anthropic_msg(msg: &Value, dst: &mut Vec<Value>) {
    let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
    let Some(content) = msg.get("content") else {
        return;
    };
    if let Some(s) = content.as_str() {
        dst.push(json!({ "role": role, "content": s }));
        return;
    }
    let Some(blocks) = content.as_array() else {
        return;
    };

    if role == "user" {
        let mut text_parts = Vec::new();
        for b in blocks {
            match b.get("type").and_then(Value::as_str) {
                Some("tool_result") => {
                    let id = b.get("tool_use_id").and_then(Value::as_str).unwrap_or_default();
                    let body = b.get("content").map(flatten_text_value).unwrap_or_default();
                    dst.push(json!({ "role": "tool", "tool_call_id": id, "content": body }));
                }
                Some("text") => {
                    if let Some(t) = b.get("text").and_then(Value::as_str) {
                        text_parts.push(t);
                    }
                }
                _ => {}
            }
        }
        if !text_parts.is_empty() {
            dst.push(json!({ "role": "user", "content": text_parts.join("\n") }));
        }
    } else if role == "assistant" {
        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();
        for b in blocks {
            match b.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(t) = b.get("text").and_then(Value::as_str) {
                        text_parts.push(t);
                    }
                }
                Some("tool_use") => {
                    let id = b.get("id").and_then(Value::as_str).unwrap_or("call_0");
                    let name = b.get("name").and_then(Value::as_str).unwrap_or_default();
                    let args = b
                        .get("input")
                        .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".to_owned()))
                        .unwrap_or_else(|| "{}".to_owned());
                    tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": { "name": name, "arguments": args }
                    }));
                }
                _ => {}
            }
        }
        let mut asst = Map::new();
        asst.insert("role".to_owned(), Value::String("assistant".to_owned()));
        if !text_parts.is_empty() {
            asst.insert("content".to_owned(), Value::String(text_parts.join("\n")));
        } else if tool_calls.is_empty() {
            asst.insert("content".to_owned(), Value::String(String::new()));
        } else {
            asst.insert("content".to_owned(), Value::Null);
        }
        if !tool_calls.is_empty() {
            asst.insert("tool_calls".to_owned(), Value::Array(tool_calls));
        }
        dst.push(Value::Object(asst));
    }
}

/// Pure Sans-I/O translator: non-streaming OpenAI ChatCompletion JSON -> Anthropic Message JSON.
pub fn translate_openai_to_anthropic_response(
    raw_json: &[u8],
    fallback_model: &str,
) -> Result<Vec<u8>, CoreError> {
    let root: Value = serde_json::from_slice(raw_json)?;
    let id = root.get("id").and_then(Value::as_str).unwrap_or("msg_aphelion");
    let model = root.get("model").and_then(Value::as_str).unwrap_or(fallback_model);
    let choice = root.get("choices").and_then(Value::as_array).and_then(|a| a.first());
    let msg = choice.and_then(|c| c.get("message"));
    let finish = choice.and_then(|c| c.get("finish_reason")).and_then(Value::as_str);

    let mut content = Vec::new();
    if let Some(rc) = msg.and_then(|m| m.get("reasoning_content")).and_then(Value::as_str) {
        if !rc.is_empty() {
            content.push(json!({ "type": "thinking", "thinking": rc }));
        }
    }
    if let Some(txt) = msg.and_then(|m| m.get("content")).and_then(Value::as_str) {
        if !txt.is_empty() {
            content.push(json!({ "type": "text", "text": txt }));
        }
    }
    if let Some(calls) = msg.and_then(|m| m.get("tool_calls")).and_then(Value::as_array) {
        for c in calls {
            let cid = c.get("id").and_then(Value::as_str).unwrap_or("tu_0");
            let name = c.pointer("/function/name").and_then(Value::as_str).unwrap_or_default();
            let raw_args = c.pointer("/function/arguments").and_then(Value::as_str).unwrap_or("{}");
            let input: Value = serde_json::from_str(raw_args).unwrap_or_else(|_| json!({}));
            content.push(json!({ "type": "tool_use", "id": cid, "name": name, "input": input }));
        }
    }

    let stop_reason = match finish {
        Some("tool_calls") => "tool_use",
        Some("length") => "max_tokens",
        _ => "end_turn",
    };
    let u = root.get("usage");
    let in_tok = u.and_then(|v| v.get("prompt_tokens")).and_then(Value::as_u64).unwrap_or(0);
    let out_tok = u.and_then(|v| v.get("completion_tokens")).and_then(Value::as_u64).unwrap_or(0);
    let cached = u
        .and_then(|v| v.pointer("/prompt_tokens_details/cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    let resp = json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": {
            "input_tokens": in_tok.saturating_sub(cached),
            "cache_read_input_tokens": cached,
            "output_tokens": out_tok
        }
    });
    Ok(serde_json::to_vec(&resp)?)
}

fn flatten_text_value(val: &Value) -> String {
    if let Some(s) = val.as_str() {
        return s.to_owned();
    }
    if let Some(arr) = val.as_array() {
        return arr
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
    }
    String::new()
}
