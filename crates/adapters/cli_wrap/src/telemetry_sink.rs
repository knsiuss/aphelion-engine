use crate::headers::extract_model;
use crate::proxy::ProxyState;
use aphelion_core::types::{ExecutionProfile, RequestOptions, SlmRouteDecision, UpstreamUsage};
use aphelion_telemetry::audit::AuditRecord;
use aphelion_telemetry::bpe::count_payload_tokens;
use aphelion_telemetry::ledger::{PricingRates, TurnTelemetry};
use bytes::Bytes;
use std::convert::Infallible;
use std::sync::atomic::Ordering;
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
    pub compaction_triggered: bool,
    pub compaction_tokens_reclaimed: usize,
    pub prompt_text: String,
    pub started_at: Instant,
}

pub fn prepare_outbound(
    state: &ProxyState,
    raw_body: &[u8],
    opts: RequestOptions,
) -> (Vec<u8>, Option<PendingTurn>) {
    let model = extract_model(raw_body);
    let raw_tok = count_payload_tokens(&model, raw_body);
    let Ok(mut guard) = state.session.lock() else {
        return (raw_body.to_vec(), None);
    };
    guard.last_usage = None;
    guard.request_options = opts;

    if let (Some(_), Some(audit_lock)) = (guard.session_id, &state.audit) {
        let key = guard.ledger_key(&model);
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
        let key = guard.ledger_key(&model);
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
    let st = &transformed.metrics.stage_stats;
    if st.macro_compaction_triggered {
        state
            .metrics
            .compactions_total
            .fetch_add(1, Ordering::Relaxed);
    }
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
        circuit_breaker_tripped: st.circuit_breaker_tripped,
        compaction_triggered: st.macro_compaction_triggered,
        compaction_tokens_reclaimed: st.compaction_tokens_reclaimed,
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

    state
        .metrics
        .uncached_input_tokens
        .fetch_add(uncached as u64, Ordering::Relaxed);
    state
        .metrics
        .cached_read_tokens
        .fetch_add(cache_read as u64, Ordering::Relaxed);
    state
        .metrics
        .output_tokens
        .fetch_add(u.output_tokens as u64, Ordering::Relaxed);
    state
        .metrics
        .reasoning_tokens
        .fetch_add(u.reasoning_tokens as u64, Ordering::Relaxed);

    let mut breaker_tripped = false;
    if let Ok(mut guard) = state.session.lock() {
        if u.input_tokens > 0 {
            guard.record_upstream_usage(&u);
        }
        let key = guard.ledger_key(&turn.model);
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
                compaction_triggered: turn.compaction_triggered,
                compaction_tokens_reclaimed: turn.compaction_tokens_reclaimed,
                prompt_text: turn.prompt_text,
                response_text,
            });
        }
    }
}
