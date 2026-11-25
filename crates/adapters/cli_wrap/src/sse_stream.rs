use crate::proxy::ProxyState;
use crate::public_guard::GuardRejection;
use crate::telemetry_sink::{finalize_turn, BoxBody, PendingTurn};
use aphelion_core::patcher::reconstruct_from_patch;
use aphelion_core::sse_bridge::OpenAiToAnthropicSseBridge;
use aphelion_core::stream_fsm::StreamFsm;
use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::{Full, StreamBody};
use hyper::body::Frame;
use hyper::{Response, StatusCode};
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

pub fn reject_response(rej: GuardRejection, state: &ProxyState) -> Response<BoxBody> {
    if rej.status == StatusCode::TOO_MANY_REQUESTS {
        state.metrics.record_rate_limited();
    }
    let mut b = Response::builder()
        .status(rej.status)
        .header("content-type", "application/json");
    if let Some(secs) = rej.retry_after_secs {
        b = b.header("retry-after", secs.to_string());
    }
    b.body(BoxBody::new(Full::new(Bytes::from(format!(
        r#"{{"error":"{}"}}"#,
        rej.message
    )))))
    .expect("reject response")
}

pub fn static_resp(status: StatusCode, content_type: &str, body: String) -> Response<BoxBody> {
    Response::builder()
        .status(status)
        .header("content-type", content_type)
        .body(BoxBody::new(Full::new(Bytes::from(body))))
        .expect("static response")
}

/// Copies upstream rate-limit, retry-after, and request-id headers onto the outbound response builder.
pub fn copy_upstream_headers(
    mut builder: hyper::http::response::Builder,
    upstream_headers: &reqwest::header::HeaderMap,
) -> hyper::http::response::Builder {
    for (name, val) in upstream_headers {
        let lower = name.as_str().to_ascii_lowercase();
        if lower == "retry-after"
            || lower == "request-id"
            || lower == "x-request-id"
            || lower.starts_with("x-ratelimit-")
            || lower.starts_with("anthropic-ratelimit-")
            || lower.starts_with("ratelimit-")
        {
            if let Ok(v) = val.to_str() {
                builder = builder.header(name.as_str(), v);
            }
        }
    }
    builder
}

pub fn apply_non_streaming_patch(json: &mut serde_json::Value, state: &ProxyState) -> bool {
    let Some(content) = json
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .filter(|s| s.trim_start().starts_with("@patch:@block:"))
        .map(str::to_owned)
    else {
        return false;
    };
    let Ok(guard) = state.session.lock() else {
        return false;
    };
    if let Some(reconstructed) = reconstruct_from_patch(&content, &guard.known_blocks) {
        if let Some(slot) = json.pointer_mut("/choices/0/message/content") {
            *slot = serde_json::Value::String(reconstructed);
            return true;
        }
    }
    false
}

pub fn stream_sse_response(
    status: StatusCode,
    content_type: String,
    upstream_res: reqwest::Response,
    state: Arc<ProxyState>,
    pending: Option<PendingTurn>,
    bridge_oai_to_anthropic: bool,
) -> Response<BoxBody> {
    let fsm = Arc::new(Mutex::new(StreamFsm::default()));
    let bridge_model = pending.as_ref().map(|p| p.model.clone()).unwrap_or_default();
    let bridge = bridge_oai_to_anthropic.then(|| Arc::new(Mutex::new(OpenAiToAnthropicSseBridge::new(bridge_model))));
    let pending_cell = Arc::new(Mutex::new(pending));
    let ttft_cell = Arc::new(Mutex::new(None::<u64>));

    let upstream_headers = upstream_res.headers().clone();
    let byte_stream = upstream_res.bytes_stream().then(move |item| {
        let (state, fsm, bridge, pending_cell, ttft_cell) = (
            Arc::clone(&state),
            Arc::clone(&fsm),
            bridge.clone(),
            Arc::clone(&pending_cell),
            Arc::clone(&ttft_cell),
        );
        async move {
            let chunk = match item {
                Ok(b) => b,
                Err(err) => {
                    eprintln!("[aphelion] upstream SSE stream error: {err}");
                    if let Ok(mut p) = pending_cell.lock() {
                        if let Some(turn) = p.take() {
                            let usage = state.session.lock().ok().and_then(|s| s.last_usage.clone());
                            let total_ms = (turn.started_at.elapsed().as_millis() as u64).max(1);
                            let ttft_ms = ttft_cell.lock().ok().and_then(|t| *t).unwrap_or(total_ms);
                            finalize_turn(&state, turn, usage, 502, ttft_ms, total_ms);
                        }
                    }
                    Bytes::new()
                }
            };
            if !chunk.is_empty() {
                if let Ok(mut t) = ttft_cell.lock() {
                    if t.is_none() {
                        if let Ok(p) = pending_cell.lock() {
                            if let Some(ref turn) = *p {
                                let ms = (turn.started_at.elapsed().as_millis() as u64).max(1);
                                *t = Some(ms);
                            }
                        }
                    }
                }
            }
            let bridged: Vec<u8> = if let Some(ref br) = bridge {
                br.lock().map(|mut b| b.push_chunk(&chunk)).unwrap_or_else(|_| chunk.to_vec())
            } else {
                chunk.to_vec()
            };
            let processed = if let (Ok(mut f), Ok(mut s)) = (fsm.lock(), state.session.lock()) {
                aphelion_core::process_sse_chunk(&mut f, &bridged, &mut s)
            } else {
                bridged
            };
            let is_terminal = processed.windows(6).any(|w| w == b"[DONE]")
                || processed.windows(12).any(|w| w == b"message_stop")
                || processed.windows(11).any(|w| w == b"\"done\":true");
            if is_terminal {
                if let Ok(mut p) = pending_cell.lock() {
                    if let Some(turn) = p.take() {
                        let usage = state.session.lock().ok().and_then(|s| s.last_usage.clone());
                        let total_ms = (turn.started_at.elapsed().as_millis() as u64).max(1);
                        let ttft_ms = ttft_cell.lock().ok().and_then(|t| *t).unwrap_or(total_ms);
                        finalize_turn(&state, turn, usage, status.as_u16(), ttft_ms, total_ms);
                    }
                }
            }
            Ok::<Frame<Bytes>, Infallible>(Frame::data(Bytes::from(processed)))
        }
    });
    let boxed: Pin<Box<dyn futures_util::Stream<Item = Result<Frame<Bytes>, Infallible>> + Send>> =
        Box::pin(byte_stream);
    copy_upstream_headers(
        Response::builder().status(status).header("content-type", content_type),
        &upstream_headers,
    )
    .body(BoxBody::new(StreamBody::new(boxed)))
    .expect("sse response builder")
}
