use crate::headers::extract_model;
use crate::proxy::ProxyState;
use crate::ui_state::TelemetryEvent;
use aphelion_core::flight_recorder::{compute_turn_net_saved_usd, normalize_provider_label};
use aphelion_core::types::{ExecutionProfile, RequestOptions, SlmRouteDecision, UpstreamUsage};
use aphelion_telemetry::audit::AuditRecord;
use aphelion_telemetry::bpe::count_payload_tokens;
use aphelion_telemetry::ledger::{PricingRates, TurnTelemetry};
use bytes::Bytes;
use std::convert::Infallible;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub type BoxBody = http_body_util::combinators::UnsyncBoxBody<Bytes, Infallible>;

pub struct PendingTurn {
    pub session_id: String,
    pub turn_index: usize,
    pub client: String,
    pub provider: String,
    pub model: String,
    pub raw_tokens: usize,
    pub opt_tokens: usize,
    pub fallback_cache_read: usize,
    pub slm_routed: bool,
    pub circuit_breaker_tripped: bool,
    pub compaction_triggered: bool,
    pub compaction_tokens_reclaimed: usize,
    pub ingress_saved_tokens: usize,
    pub escalate_to_frontier: bool,
    pub started_at: Instant,
}

pub fn prepare_outbound(
    state: &ProxyState,
    raw_body: &[u8],
    opts: RequestOptions,
    provider_id: &str,
    client_tag: &str,
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
    let pending = PendingTurn {
        session_id: transformed.metrics.session_id_hex,
        turn_index: guard.turn_index,
        client: client_tag.to_owned(),
        provider: normalize_provider_label(provider_id),
        model,
        raw_tokens: raw_tok,
        opt_tokens: opt_tok,
        fallback_cache_read: fallback_cache,
        slm_routed: st.routed_fast_model.is_some()
            || matches!(transformed.metrics.slm_route, SlmRouteDecision::CompiledSlm { .. }),
        circuit_breaker_tripped: st.circuit_breaker_tripped,
        compaction_triggered: st.macro_compaction_triggered,
        compaction_tokens_reclaimed: st.compaction_tokens_reclaimed,
        ingress_saved_tokens: st.ingress_saved_tokens,
        escalate_to_frontier: transformed.metrics.complexity_deep || st.circuit_breaker_tripped,
        started_at: Instant::now(),
    };
    (transformed.body, Some(pending))
}

pub fn finalize_turn(
    state: &ProxyState,
    turn: PendingTurn,
    usage: Option<UpstreamUsage>,
    status_code: u16,
    ttft_ms: u64,
    total_ms: u64,
) {
    let u = usage.unwrap_or_default();
    let cache_read = if u.cached_tokens > 0 {
        u.cached_tokens
    } else if (200..=299).contains(&status_code) {
        turn.fallback_cache_read
    } else {
        0
    };
    let uncached = if u.input_tokens > 0 {
        u.uncached_input_tokens
    } else if (200..=299).contains(&status_code) {
        turn.opt_tokens.saturating_sub(cache_read)
    } else {
        0
    };

    state.metrics.record_request(
        &turn.provider,
        &turn.model,
        status_code,
        ttft_ms,
        cache_read as u64,
        uncached as u64,
        u.output_tokens as u64,
        u.reasoning_tokens as u64,
        turn.compaction_triggered,
    );

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

    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let ingress_saved = turn.ingress_saved_tokens.max(turn.raw_tokens.saturating_sub(turn.opt_tokens));
    let net_saved_usd = compute_turn_net_saved_usd(
        turn.turn_index, turn.raw_tokens, turn.opt_tokens, uncached, cache_read,
        u.cache_creation_tokens, u.output_tokens, c_eff,
    );
    let denom = cache_read + uncached + u.cache_creation_tokens;
    let cache_hit_pct = if denom == 0 { 0.0 } else { (cache_read as f64 * 100.0) / (denom as f64) };
    let mut badges = Vec::new();
    if turn.compaction_triggered {
        badges.push("COMPACTED".to_owned());
    }
    if turn.circuit_breaker_tripped {
        badges.push("LOOP_STOPPED".to_owned());
    }

    let ev = TelemetryEvent {
        timestamp_ms,
        client: turn.client,
        provider: turn.provider.clone(),
        model: turn.model.clone(),
        status_code,
        ttft_ms,
        cache_hit_pct,
        cached_in: cache_read,
        uncached_in: uncached,
        output: u.output_tokens,
        reasoning: u.reasoning_tokens,
        net_saved_usd,
        badges,
    };
    if let Ok(mut topo) = state.topology.lock() {
        topo.record(ev.clone());
    }
    let _ = state.events_tx.send(ev);

    if let Some(audit_lock) = &state.audit {
        if let Ok(db) = audit_lock.lock() {
            let _ = db.record(&AuditRecord {
                timestamp_ms,
                session_id: turn.session_id,
                turn_index: turn.turn_index,
                provider: turn.provider,
                model: turn.model,
                status_code,
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
                ingress_tokens_saved: ingress_saved,
                net_saved_usd,
                prompt_text: String::new(),
                response_text: String::new(),
            });
        }
    }
}
