use crate::proxy::ProxyState;
use crate::ui_state::TelemetryEvent;
use aphelion_core::flight_recorder::AuditFilter;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub fn build_overview_json(state: &ProxyState) -> String {
    let policy = state.policy.read().map(|p| p.clone()).unwrap_or_default();
    let recent: Vec<TelemetryEvent> = state
        .topology
        .lock()
        .map(|t| t.recent.iter().cloned().collect())
        .unwrap_or_default();

    let (summary, groups, records) = if let Some(ref lock) = state.audit {
        if let Ok(db) = lock.lock() {
            (
                db.summarize().unwrap_or_default(),
                db.group_stats(&AuditFilter::default()).unwrap_or_default(),
                db.load_all().unwrap_or_default(),
            )
        } else {
            Default::default()
        }
    } else {
        Default::default()
    };

    let avg_ttft = if records.is_empty() {
        0
    } else {
        records.iter().map(|r| r.ttft_ms).sum::<u64>() / (records.len() as u64)
    };
    let mut buckets: BTreeMap<i64, [usize; 4]> = BTreeMap::new();
    for r in &records {
        let hour = (r.timestamp_ms / 3_600_000) * 3_600_000;
        let b = buckets.entry(hour).or_insert([0; 4]);
        b[0] += r.cache_read_tokens;
        b[1] += r.uncached_input_tokens;
        b[2] += r.output_tokens;
        b[3] += r.reasoning_tokens;
    }
    let timeseries: Vec<Value> = buckets
        .into_iter()
        .rev()
        .take(24)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|(ts, b)| json!({"hour_ms": ts, "cached_in": b[0], "uncached_in": b[1], "output": b[2], "reasoning": b[3]}))
        .collect();

    let by_pm: Vec<Value> = groups
        .iter()
        .map(|g| {
            let errs = records
                .iter()
                .filter(|r| r.provider == g.provider && r.model == g.model && r.status_code >= 400)
                .count();
            let err_pct = if g.requests == 0 {
                0.0
            } else {
                (errs as f64 * 100.0) / (g.requests as f64)
            };
            json!({
                "provider": g.provider, "model": g.model, "req": g.requests,
                "avg_ttft_ms": g.avg_ttft_ms(), "cache_hit_pct": g.cache_hit_pct(),
                "cached_in": g.cached_read_tokens, "uncached_in": g.uncached_input_tokens,
                "output": g.output_tokens, "reasoning": g.reasoning_tokens,
                "net_saved_usd": g.net_saved_usd, "error_rate_pct": err_pct
            })
        })
        .collect();

    json!({
        "kpis": {
            "total_requests": summary.total_requests,
            "verified_cache_hit_pct": summary.cache_hit_pct(),
            "total_net_saved_usd": summary.net_usd_saved(),
            "cached_read_tokens": summary.total_cache_read_tokens,
            "uncached_input_tokens": summary.total_uncached_input_tokens,
            "output_tokens": summary.total_output_tokens,
            "reasoning_tokens": summary.total_reasoning_tokens,
            "compactions_count": summary.total_compactions,
            "avg_ttft_ms": avg_ttft
        },
        "by_provider_model": by_pm,
        "timeseries": timeseries,
        "policy": policy,
        "recent": recent
    })
    .to_string()
}

pub fn build_topology_json(state: &ProxyState) -> String {
    let policy = state.policy.read().map(|p| p.clone()).unwrap_or_default();
    let (clients, upstreams) = state
        .topology
        .lock()
        .map(|t| {
            let c: Vec<Value> = t.clients.iter().map(|(k, v)| json!({"name": k, "requests": v})).collect();
            let u: Vec<Value> = t.upstreams.values().map(|n| json!(n)).collect();
            (c, u)
        })
        .unwrap_or_default();
    let (sessions, hit_pct) = state
        .audit
        .as_ref()
        .and_then(|l| l.lock().ok())
        .and_then(|db| db.summarize().ok())
        .map(|s| (s.total_requests.min(1).max(clients.len()), s.cache_hit_pct()))
        .unwrap_or((0, 0.0));
    json!({
        "clients": clients,
        "engine": {
            "active_sessions": sessions,
            "global_hit_pct": hit_pct,
            "output_mode": policy.output_mode,
            "compaction_mode": policy.compaction_mode,
            "reasoning_effort": policy.reasoning_effort
        },
        "upstreams": upstreams,
        "uptime_secs": state.started_at.elapsed().as_secs()
    })
    .to_string()
}
