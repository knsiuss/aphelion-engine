use crate::headers::extract_model;
use crate::proxy::ProxyState;
use aphelion_core::patcher::reconstruct_from_patch;
use aphelion_core::prefix_ledger::PrefixLedger;
use aphelion_core::stream_fsm::StreamFsm;
use aphelion_core::types::{ExecutionProfile, SlmRouteDecision, UpstreamUsage};
use aphelion_telemetry::audit::AuditRecord;
use aphelion_telemetry::bpe::count_payload_tokens;
use aphelion_telemetry::ledger::{PricingRates, TurnTelemetry};
use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::StreamBody;
use hyper::body::Frame;
use hyper::{Response, StatusCode};
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub type BoxBody = http_body_util::combinators::UnsyncBoxBody<Bytes, Infallible>;

pub struct PendingTurn {
    pub session_id: String,
    pub turn_index: usize,
    pub model: String,
    pub raw_tokens: usize,
    pub opt_tokens: usize,
    pub fallback_cache_read: usize,
    pub slm_routed: bool,
    pub circuit_breaker_tripped: bool,
    pub prompt_text: String,
    pub started_at: Instant,
}

pub fn prepare_outbound(state: &ProxyState, raw_body: &[u8]) -> (Vec<u8>, Option<PendingTurn>) {
    let model = extract_model(raw_body);
    let raw_tok = count_payload_tokens(&model, raw_body);
    let Ok(mut guard) = state.session.lock() else {
        return (raw_body.to_vec(), None);
    };
    guard.last_usage = None;

    // Restore persisted PrefixLedger entry from SQLite if present.
    if let (Some(sid), Some(audit_lock)) = (guard.session_id, &state.audit) {
        let key = PrefixLedger::active_key(Some(sid), &model);
        if guard.prefix_ledger.get(&key).is_none() {
            if let Ok(db) = audit_lock.lock() {
                if let Some(saved) = db.load_prefix_record(&key) {
                    guard.prefix_ledger.restore_record(key, saved);
                }
            }
        }
    }

    let Ok(transformed) =
        aphelion_core::prepare_request(raw_body, ExecutionProfile::Ephemeral, &mut guard)
    else {
        return (raw_body.to_vec(), None);
    };

    if let Some(audit_lock) = &state.audit {
        let key = PrefixLedger::active_key(guard.session_id, &model);
        if let Some(rec) = guard.prefix_ledger.get(&key) {
            if let Ok(db) = audit_lock.lock() {
                let _ = db.save_prefix_record(&key, rec);
            }
        }
    }

    let opt_tok = count_payload_tokens(&model, &transformed.body);
    let fallback_cache = if guard.turn_index > 1 {
        count_payload_tokens(&model, &guard.frozen_prefix_bytes)
    } else {
        0
    };
    let pending = PendingTurn {
        session_id: transformed.metrics.session_id_hex,
        turn_index: guard.turn_index,
        model,
        raw_tokens: raw_tok,
        opt_tokens: opt_tok,
        fallback_cache_read: fallback_cache,
        slm_routed: matches!(
            transformed.metrics.slm_route,
            SlmRouteDecision::CompiledSlm { .. }
        ),
        circuit_breaker_tripped: transformed.metrics.stage_stats.circuit_breaker_tripped,
        prompt_text: String::from_utf8_lossy(raw_body).into_owned(),
        started_at: Instant::now(),
    };
    (transformed.body, Some(pending))
}

pub fn finalize_turn(
    state: &ProxyState,
    turn: PendingTurn,
    usage: Option<UpstreamUsage>,
    ttft_ms: u64,
    total_ms: u64,
    response_text: String,
) {
    let u = usage.unwrap_or_default();
    let cache_read = if u.cached_tokens > 0 {
        u.cached_tokens
    } else {
        turn.fallback_cache_read
    };
    let uncached = if u.input_tokens > 0 {
        u.uncached_input_tokens
    } else {
        turn.opt_tokens.saturating_sub(cache_read)
    };

    let mut breaker_tripped = false;
    if let Ok(mut guard) = state.session.lock() {
        if u.input_tokens > 0 {
            guard.record_upstream_usage(&u);
        }
        let key = PrefixLedger::active_key(guard.session_id, &turn.model);
        if let Some(rec) = guard.prefix_ledger.get(&key) {
            breaker_tripped = rec.cache_drop_breaker_tripped;
            if let Some(audit_lock) = &state.audit {
                if let Ok(db) = audit_lock.lock() {
                    let _ = db.save_prefix_record(&key, rec);
                }
            }
        }
    }

    let tel = TurnTelemetry {
        raw_input_tokens: turn.raw_tokens,
        opt_input_tokens: turn.opt_tokens,
        cache_write_tokens: u.cache_creation_tokens,
        cache_read_tokens: cache_read,
        think_output_tokens: u.reasoning_tokens,
        action_output_tokens: u.output_tokens.saturating_sub(u.reasoning_tokens),
        ..TurnTelemetry::default()
    };
    let rates = PricingRates::default();
    let c_eff = tel.effective_cost_usd(rates);
    if let Ok(mut sum) = state.summary.lock() {
        sum.record(&tel, rates);
    }
    if let Some(audit_lock) = &state.audit {
        if let Ok(db) = audit_lock.lock() {
            let _ = db.record(&AuditRecord {
                session_id: turn.session_id,
                turn_index: turn.turn_index,
                model: turn.model,
                raw_tokens: turn.raw_tokens,
                opt_tokens: turn.opt_tokens,
                uncached_input_tokens: uncached,
                cache_read_tokens: cache_read,
                cache_creation_tokens: u.cache_creation_tokens,
                output_tokens: u.output_tokens,
                reasoning_tokens: u.reasoning_tokens,
                ttft_ms,
                total_ms,
                c_eff_usd: c_eff,
                slm_routed: turn.slm_routed,
                circuit_breaker_tripped: turn.circuit_breaker_tripped,
                cache_drop_breaker_tripped: breaker_tripped,
                prompt_text: turn.prompt_text,
                response_text,
            });
        }
    }
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
) -> Response<BoxBody> {
    let fsm = Arc::new(Mutex::new(StreamFsm::default()));
    let pending_cell = Arc::new(Mutex::new(pending));
    let ttft_cell = Arc::new(Mutex::new(None::<u64>));

    let byte_stream = upstream_res.bytes_stream().then(move |item| {
        let (state, fsm, pending_cell, ttft_cell) = (
            Arc::clone(&state),
            Arc::clone(&fsm),
            Arc::clone(&pending_cell),
            Arc::clone(&ttft_cell),
        );
        async move {
            let chunk = item.unwrap_or_default();
            if let Ok(mut t) = ttft_cell.lock() {
                if t.is_none() {
                    if let Ok(p) = pending_cell.lock() {
                        if let Some(ref turn) = *p {
                            *t = Some(turn.started_at.elapsed().as_millis() as u64);
                        }
                    }
                }
            }
            let processed = if let (Ok(mut f), Ok(mut s)) = (fsm.lock(), state.session.lock()) {
                aphelion_core::process_sse_chunk(&mut f, &chunk, &mut s)
            } else {
                chunk.to_vec()
            };
            let is_terminal = processed.windows(6).any(|w| w == b"[DONE]")
                || processed.windows(12).any(|w| w == b"message_stop")
                || processed.windows(11).any(|w| w == b"\"done\":true");
            if is_terminal {
                if let Ok(mut p) = pending_cell.lock() {
                    if let Some(turn) = p.take() {
                        let usage = state.session.lock().ok().and_then(|s| s.last_usage.clone());
                        let total_ms = turn.started_at.elapsed().as_millis() as u64;
                        let ttft_ms = ttft_cell.lock().ok().and_then(|t| *t).unwrap_or(total_ms);
                        finalize_turn(&state, turn, usage, ttft_ms, total_ms, String::new());
                    }
                }
            }
            Ok::<Frame<Bytes>, Infallible>(Frame::data(Bytes::from(processed)))
        }
    });
    let boxed: Pin<Box<dyn futures_util::Stream<Item = Result<Frame<Bytes>, Infallible>> + Send>> =
        Box::pin(byte_stream);
    Response::builder()
        .status(status)
        .header("content-type", content_type)
        .body(BoxBody::new(StreamBody::new(boxed)))
        .expect("sse response builder")
}
