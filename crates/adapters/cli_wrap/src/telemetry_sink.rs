use crate::headers::extract_model;
use crate::proxy::ProxyState;
use aphelion_core::types::{ExecutionProfile, SlmRouteDecision, UpstreamUsage};
use aphelion_telemetry::audit::AuditRecord;
use aphelion_telemetry::bpe::count_payload_tokens;
use aphelion_telemetry::ledger::{PricingRates, TurnTelemetry};
use std::time::Instant;

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
    let Ok(transformed) =
        aphelion_core::prepare_request(raw_body, ExecutionProfile::Ephemeral, &mut guard)
    else {
        return (raw_body.to_vec(), None);
    };
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
                cache_read_tokens: cache_read,
                cache_creation_tokens: u.cache_creation_tokens,
                output_tokens: u.output_tokens,
                reasoning_tokens: u.reasoning_tokens,
                ttft_ms,
                total_ms,
                c_eff_usd: c_eff,
                slm_routed: turn.slm_routed,
                circuit_breaker_tripped: turn.circuit_breaker_tripped,
                prompt_text: turn.prompt_text,
                response_text,
            });
        }
    }
}
