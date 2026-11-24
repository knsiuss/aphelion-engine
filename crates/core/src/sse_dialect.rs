use crate::stream_fsm::{FsmState, StreamFsm};
use crate::types::{SessionState, UpstreamUsage};
use crate::usage_extract::extract_usage_universal;
use serde_json::{json, Value};

/// Backwards-compatible alias for non-streaming JSON usage extraction.
pub fn extract_usage_from_json(root: &Value) -> Option<UpstreamUsage> {
    extract_usage_universal(root)
}

/// Dispatches a complete SSE frame (`data: ...\n\n`) or Ollama NDJSON line (`{...}\n`)
/// across OpenAI Chat, OpenAI Responses, DeepSeek, Anthropic, Gemini, and Ollama.
pub fn process_sse_frame(fsm: &mut StreamFsm, frame: &[u8], session: &mut SessionState) -> Vec<u8> {
    let Ok(frame_str) = std::str::from_utf8(frame) else {
        return frame.to_vec();
    };
    let trimmed = frame_str.trim();

    // Ollama NDJSON line (raw JSON object without `data:` prefix)
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        if let Ok(root) = serde_json::from_str::<Value>(trimmed) {
            if let Some(u) = extract_usage_universal(&root) {
                session.last_usage = Some(u);
            }
        }
        return frame.to_vec();
    }

    let Some(data_line) = frame_str.lines().find(|l| l.starts_with("data:")) else {
        return frame.to_vec();
    };
    let payload = data_line.trim_start_matches("data:").trim();

    if payload == "[DONE]" {
        return flush_before_terminal(fsm, session, frame, false);
    }

    let Ok(mut root) = serde_json::from_str::<Value>(payload) else {
        return frame.to_vec();
    };

    let ev_type = root.get("type").and_then(Value::as_str).map(str::to_owned);
    if ev_type.as_deref() != Some("message_delta") {
        if let Some(usage) = extract_usage_universal(&root) {
            session.last_usage = Some(usage);
        }
    }

    // Google Gemini SSE (`candidates`)
    if root.get("candidates").is_some() {
        return frame.to_vec();
    }

    // OpenAI Responses API & Anthropic SSE events (`type`)
    if let Some(ev_type) = ev_type {
        if ev_type.starts_with("response.") {
            return handle_openai_responses_event(fsm, &ev_type, &mut root, frame, session);
        }
        return handle_anthropic_event(fsm, &ev_type, &mut root, frame, session);
    }

    handle_openai_chunk(fsm, &mut root, frame, session)
}

fn handle_openai_responses_event(
    fsm: &mut StreamFsm,
    ev_type: &str,
    root: &mut Value,
    raw_frame: &[u8],
    session: &mut SessionState,
) -> Vec<u8> {
    if ev_type == "response.output_text.delta" {
        if let Some(delta) = root.get("delta").and_then(Value::as_str).map(str::to_owned) {
            let transformed = fsm.process_text_delta(delta.as_bytes(), session);
            if transformed.is_empty() {
                return Vec::new();
            }
            if let Ok(new_text) = String::from_utf8(transformed) {
                root["delta"] = Value::String(new_text);
                let prefix = extract_event_line(raw_frame);
                return format!("{prefix}data: {root}\n\n").into_bytes();
            }
        }
    }
    raw_frame.to_vec()
}

fn handle_openai_chunk(
    fsm: &mut StreamFsm,
    root: &mut Value,
    raw_frame: &[u8],
    session: &mut SessionState,
) -> Vec<u8> {
    let has_tool_delta = root
        .pointer("/choices/0/delta/tool_calls")
        .and_then(Value::as_array)
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    if has_tool_delta {
        fsm.force_passthrough();
        return raw_frame.to_vec();
    }

    if let Some(reasoning) = root
        .pointer("/choices/0/delta/reasoning_content")
        .and_then(Value::as_str)
    {
        fsm.ingest_reasoning_delta(reasoning.as_bytes(), session);
        if root
            .pointer("/choices/0/delta/content")
            .and_then(Value::as_str)
            .is_none()
        {
            return raw_frame.to_vec();
        }
    }

    let Some(content) = root
        .pointer("/choices/0/delta/content")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return raw_frame.to_vec();
    };

    fsm.finalize_reasoning_if_active(session);
    let transformed = fsm.process_text_delta(content.as_bytes(), session);
    if transformed.is_empty() {
        return Vec::new();
    }
    let Ok(new_text) = String::from_utf8(transformed) else {
        return raw_frame.to_vec();
    };
    if let Some(slot) = root.pointer_mut("/choices/0/delta/content") {
        *slot = Value::String(new_text);
    }
    format!("data: {root}\n\n").into_bytes()
}

fn handle_anthropic_event(
    fsm: &mut StreamFsm,
    ev_type: &str,
    root: &mut Value,
    raw_frame: &[u8],
    session: &mut SessionState,
) -> Vec<u8> {
    match ev_type {
        "message_start" => {
            if let Some(u) = root.get("message").and_then(extract_usage_universal) {
                session.last_usage = Some(u);
            }
            return raw_frame.to_vec();
        }
        "message_delta" => {
            if let Some(out_tok) = root.pointer("/usage/output_tokens").and_then(Value::as_u64) {
                session
                    .last_usage
                    .get_or_insert_with(UpstreamUsage::default)
                    .output_tokens = out_tok as usize;
            }
            return raw_frame.to_vec();
        }
        "message_stop" => return flush_before_terminal(fsm, session, raw_frame, true),
        "content_block_delta" => {}
        _ => return raw_frame.to_vec(),
    }

    match root
        .pointer("/delta/type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "thinking_delta" => {
            if let Some(thinking) = root.pointer("/delta/thinking").and_then(Value::as_str) {
                fsm.ingest_reasoning_delta(thinking.as_bytes(), session);
            }
            raw_frame.to_vec()
        }
        "input_json_delta" => {
            fsm.finalize_reasoning_if_active(session);
            fsm.force_passthrough();
            raw_frame.to_vec()
        }
        "text_delta" => {
            fsm.finalize_reasoning_if_active(session);
            let Some(text) = root
                .pointer("/delta/text")
                .and_then(Value::as_str)
                .map(str::to_owned)
            else {
                return raw_frame.to_vec();
            };
            let transformed = fsm.process_text_delta(text.as_bytes(), session);
            if transformed.is_empty() {
                return Vec::new();
            }
            let Ok(new_text) = String::from_utf8(transformed) else {
                return raw_frame.to_vec();
            };
            if let Some(slot) = root.pointer_mut("/delta/text") {
                *slot = Value::String(new_text);
            }
            let prefix = extract_event_line(raw_frame);
            format!("{prefix}data: {root}\n\n").into_bytes()
        }
        _ => raw_frame.to_vec(),
    }
}

fn flush_before_terminal(
    fsm: &mut StreamFsm,
    session: &mut SessionState,
    terminal_frame: &[u8],
    anthropic: bool,
) -> Vec<u8> {
    fsm.finalize_reasoning_if_active(session);
    let was_patching = fsm.state == FsmState::PatchBuffer;
    let flushed = fsm.flush_text_buffers(session);
    let mut out = Vec::new();
    if !flushed.is_empty() {
        let text = String::from_utf8_lossy(&flushed);
        if anthropic {
            let synth = json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "text_delta", "text": text }
            });
            out.extend_from_slice(
                format!("event: content_block_delta\ndata: {synth}\n\n").as_bytes(),
            );
        } else if was_patching || !text.is_empty() {
            let synth = json!({ "choices": [{ "delta": { "content": text } }] });
            out.extend_from_slice(format!("data: {synth}\n\n").as_bytes());
        }
    }
    out.extend_from_slice(terminal_frame);
    out
}

fn extract_event_line(raw_frame: &[u8]) -> String {
    let Ok(s) = std::str::from_utf8(raw_frame) else {
        return String::new();
    };
    s.lines()
        .find(|l| l.starts_with("event:"))
        .map(|ev| format!("{ev}\n"))
        .unwrap_or_default()
}
