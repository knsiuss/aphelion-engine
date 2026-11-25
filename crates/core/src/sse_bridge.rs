use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
enum ActiveBlock {
    None,
    Thinking(usize),
    Text(usize),
    ToolUse { block_idx: usize, oai_index: usize },
}

/// Pure Sans-I/O streaming translator: OpenAI `chat.completion.chunk` SSE -> Anthropic Messages SSE.
/// Safe against 1-byte chunk splits across UTF-8, CRLF/LF delimiters, `reasoning_content`, and `tool_calls`.
pub struct OpenAiToAnthropicSseBridge {
    model: String,
    buf: Vec<u8>,
    started: bool,
    finished: bool,
    next_block_idx: usize,
    active: ActiveBlock,
    stop_reason: String,
    in_tokens: u64,
    cached_tokens: u64,
    out_tokens: u64,
}

impl OpenAiToAnthropicSseBridge {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            buf: Vec::new(),
            started: false,
            finished: false,
            next_block_idx: 0,
            active: ActiveBlock::None,
            stop_reason: "end_turn".to_owned(),
            in_tokens: 0,
            cached_tokens: 0,
            out_tokens: 0,
        }
    }

    pub fn push_chunk(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some((end, sep_len)) = find_sse_frame_end(&self.buf) {
            let frame: Vec<u8> = self.buf.drain(..end + sep_len).collect();
            let text = String::from_utf8_lossy(&frame[..end]);
            self.process_frame(&text, &mut out);
        }
        out
    }

    pub fn finish(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        if !self.buf.is_empty() {
            let tail = std::mem::take(&mut self.buf);
            let text = String::from_utf8_lossy(&tail);
            self.process_frame(&text, &mut out);
        }
        if self.started && !self.finished {
            self.emit_message_end(&mut out);
        }
        out
    }

    fn process_frame(&mut self, frame: &str, out: &mut Vec<u8>) {
        let mut data_lines = Vec::new();
        for line in frame.lines() {
            if let Some(d) = line.strip_prefix("data:") {
                data_lines.push(d.trim());
            }
        }
        if data_lines.is_empty() {
            return;
        }
        let payload = data_lines.join("\n");
        if payload == "[DONE]" {
            if self.started && !self.finished {
                self.emit_message_end(out);
            }
            return;
        }
        let Ok(json) = serde_json::from_str::<Value>(&payload) else {
            return;
        };
        if let Some(u) = json.get("usage") {
            if let Some(p) = u.get("prompt_tokens").and_then(Value::as_u64) {
                self.in_tokens = p;
            }
            if let Some(c) = u.get("completion_tokens").and_then(Value::as_u64) {
                self.out_tokens = c;
            }
            if let Some(cr) = u.pointer("/prompt_tokens_details/cached_tokens").and_then(Value::as_u64) {
                self.cached_tokens = cr;
            }
        }
        if !self.started {
            let id = json.get("id").and_then(Value::as_str).unwrap_or("msg_aphelion");
            let model = json.get("model").and_then(Value::as_str).unwrap_or(&self.model);
            emit_event(
                out,
                "message_start",
                &json!({
                    "type": "message_start",
                    "message": {
                        "id": id, "type": "message", "role": "assistant", "model": model,
                        "content": [], "stop_reason": null, "stop_sequence": null,
                        "usage": { "input_tokens": self.in_tokens.saturating_sub(self.cached_tokens), "output_tokens": 0 }
                    }
                }),
            );
            self.started = true;
        }

        let Some(choice) = json.get("choices").and_then(Value::as_array).and_then(|a| a.first()) else {
            return;
        };
        if let Some(fr) = choice.get("finish_reason").and_then(Value::as_str) {
            self.stop_reason = match fr {
                "tool_calls" => "tool_use".to_owned(),
                "length" => "max_tokens".to_owned(),
                _ => "end_turn".to_owned(),
            };
        }
        let Some(delta) = choice.get("delta") else {
            return;
        };
        if let Some(rc) = delta.get("reasoning_content").and_then(Value::as_str) {
            if !rc.is_empty() {
                let idx = self.ensure_thinking_block(out);
                emit_event(
                    out,
                    "content_block_delta",
                    &json!({ "type": "content_block_delta", "index": idx, "delta": { "type": "thinking_delta", "thinking": rc } }),
                );
            }
        }
        if let Some(txt) = delta.get("content").and_then(Value::as_str) {
            if !txt.is_empty() {
                let idx = self.ensure_text_block(out);
                emit_event(
                    out,
                    "content_block_delta",
                    &json!({ "type": "content_block_delta", "index": idx, "delta": { "type": "text_delta", "text": txt } }),
                );
            }
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for tc in calls {
                let oai_idx = tc.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let id = tc.get("id").and_then(Value::as_str);
                let name = tc.pointer("/function/name").and_then(Value::as_str);
                let args = tc.pointer("/function/arguments").and_then(Value::as_str).unwrap_or_default();
                let block_idx = self.ensure_tool_block(oai_idx, id, name, out);
                if !args.is_empty() {
                    emit_event(
                        out,
                        "content_block_delta",
                        &json!({ "type": "content_block_delta", "index": block_idx, "delta": { "type": "input_json_delta", "partial_json": args } }),
                    );
                }
            }
        }
    }

    fn close_active_block(&mut self, out: &mut Vec<u8>) {
        let idx = match self.active {
            ActiveBlock::None => return,
            ActiveBlock::Thinking(i) | ActiveBlock::Text(i) | ActiveBlock::ToolUse { block_idx: i, .. } => i,
        };
        emit_event(out, "content_block_stop", &json!({ "type": "content_block_stop", "index": idx }));
        self.active = ActiveBlock::None;
    }

    fn ensure_thinking_block(&mut self, out: &mut Vec<u8>) -> usize {
        if let ActiveBlock::Thinking(i) = self.active {
            return i;
        }
        self.close_active_block(out);
        let idx = self.next_block_idx;
        self.next_block_idx += 1;
        emit_event(out, "content_block_start", &json!({ "type": "content_block_start", "index": idx, "content_block": { "type": "thinking", "thinking": "" } }));
        self.active = ActiveBlock::Thinking(idx);
        idx
    }

    fn ensure_text_block(&mut self, out: &mut Vec<u8>) -> usize {
        if let ActiveBlock::Text(i) = self.active {
            return i;
        }
        self.close_active_block(out);
        let idx = self.next_block_idx;
        self.next_block_idx += 1;
        emit_event(out, "content_block_start", &json!({ "type": "content_block_start", "index": idx, "content_block": { "type": "text", "text": "" } }));
        self.active = ActiveBlock::Text(idx);
        idx
    }

    fn ensure_tool_block(&mut self, oai_index: usize, id: Option<&str>, name: Option<&str>, out: &mut Vec<u8>) -> usize {
        if let ActiveBlock::ToolUse { block_idx, oai_index: cur } = self.active {
            if cur == oai_index {
                return block_idx;
            }
        }
        self.close_active_block(out);
        let idx = self.next_block_idx;
        self.next_block_idx += 1;
        let tid = id.unwrap_or("toolu_aphelion");
        let tname = name.unwrap_or("tool");
        emit_event(out, "content_block_start", &json!({ "type": "content_block_start", "index": idx, "content_block": { "type": "tool_use", "id": tid, "name": tname, "input": {} } }));
        self.active = ActiveBlock::ToolUse { block_idx: idx, oai_index };
        idx
    }

    fn emit_message_end(&mut self, out: &mut Vec<u8>) {
        self.close_active_block(out);
        emit_event(
            out,
            "message_delta",
            &json!({
                "type": "message_delta",
                "delta": { "stop_reason": self.stop_reason, "stop_sequence": null },
                "usage": {
                    "input_tokens": self.in_tokens.saturating_sub(self.cached_tokens),
                    "cache_read_input_tokens": self.cached_tokens,
                    "output_tokens": self.out_tokens
                }
            }),
        );
        emit_event(out, "message_stop", &json!({ "type": "message_stop" }));
        self.finished = true;
    }
}

fn emit_event(out: &mut Vec<u8>, event: &str, data: &Value) {
    let s = format!("event: {event}\ndata: {}\n\n", serde_json::to_string(data).unwrap_or_default());
    out.extend_from_slice(s.as_bytes());
}

fn find_sse_frame_end(buf: &[u8]) -> Option<(usize, usize)> {
    let lf = buf.windows(2).position(|w| w == b"\n\n");
    let crlf = buf.windows(4).position(|w| w == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(a), Some(b)) => if a < b { Some((a, 2)) } else { Some((b, 4)) },
        (Some(a), None) => Some((a, 2)),
        (None, Some(b)) => Some((b, 4)),
        (None, None) => None,
    }
}
