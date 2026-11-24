use crate::stream_fsm::{FsmState, StreamFsm};
use crate::types::{SessionState, UpstreamUsage};
use serde_json::{json, Value};

/// Dispatches a complete SSE frame across OpenAI/OpenRouter/DeepSeek and Anthropic dialects,
/// extracting real upstream `usage` / `cached_tokens` into `session.last_usage`.
pub fn process_sse_frame(fsm: &mut StreamFsm, frame: &[u8], session: &mut SessionState) -> Vec<u8> {
    let Ok(frame_str) = std::str::from_utf8(frame) else {
        return frame.to_vec();
    };
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

    if let Some(ev_type) = root.get("type").and_then(Value::as_str).map(str::to_owned) {
        return handle_anthropic_event(fsm, &ev_type, &mut root, frame, session);
    }

    handle_openai_chunk(fsm, &mut root, frame, session)
}

/// Extracts real upstream usage from a non-streaming OpenAI or Anthropic JSON response.
pub fn extract_usage_from_json(root: &Value) -> Option<UpstreamUsage> {
    let u = root.get("usage")?;
    let mut out = UpstreamUsage::default();
    if let Some(pt) = u.get("prompt_tokens").and_then(Value::as_u64) {
        out.input_tokens = pt as usize;
        out.output_tokens = u.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0) as usize;
        out.cached_tokens = u
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        out.reasoning_tokens = u
            .pointer("/completion_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        return Some(out);
    }
    if u.get("input_tokens").is_some() || u.get("output_tokens").is_some() {
        out.input_tokens = u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0) as usize;
        out.output_tokens = u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0) as usize;
        out.cached_tokens = u
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        out.cache_creation_tokens = u
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        return Some(out);
    }
    None
}

fn handle_openai_chunk(
    fsm: &mut StreamFsm,
    root: &mut Value,
    raw_frame: &[u8],
    session: &mut SessionState,
) -> Vec<u8> {
    if let Some(usage) = extract_usage_from_json(root) {
        session.last_usage = Some(usage);
    }

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
        if root.pointer("/choices/0/delta/content").and_then(Value::as_str).is_none() {
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
    if ev_type == "message_start" {
        if let Some(msg) = root.get("message") {
            if let Some(u) = extract_usage_from_json(msg) {
                session.last_usage = Some(u);
            }
        }
        return raw_frame.to_vec();
    }
    if ev_type == "message_delta" {
        if let Some(out_tok) = root.pointer("/usage/output_tokens").and_then(Value::as_u64) {
            let entry = session.last_usage.get_or_insert_with(UpstreamUsage::default);
            entry.output_tokens = out_tok as usize;
        }
        return raw_frame.to_vec();
    }
    if ev_type == "message_stop" {
        return flush_before_terminal(fsm, session, raw_frame, true);
    }
    if ev_type != "content_block_delta" {
        return raw_frame.to_vec();
    }

    let delta_type = root
        .pointer("/delta/type")
        .and_then(Value::as_str)
        .unwrap_or_default();

    match delta_type {
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
    if let Some(ev) = s.lines().find(|l| l.starts_with("event:")) {
        format!("{ev}\n")
    } else {
        String::new()
    }
}
