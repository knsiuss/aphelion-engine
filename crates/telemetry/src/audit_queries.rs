use crate::audit::AuditRecord;
use crate::prefix_store::AuditSummary;
use aphelion_core::flight_recorder::{
    compute_turn_net_saved_usd_for_model, normalize_provider_label, AuditFilter, ProviderModelStats,
};
use aphelion_core::pricing::resolve_model_pricing;
use rusqlite::{Connection, Row};
use std::collections::{BTreeMap, HashSet};

const MIGRATIONS: &[(&str, &str)] = &[
    ("timestamp_ms", "INTEGER NOT NULL DEFAULT 0"),
    ("provider", "TEXT NOT NULL DEFAULT 'unknown'"),
    ("status_code", "INTEGER NOT NULL DEFAULT 200"),
    ("uncached_input_tokens", "INTEGER NOT NULL DEFAULT 0"),
    ("cache_creation_tokens", "INTEGER NOT NULL DEFAULT 0"),
    ("output_tokens", "INTEGER NOT NULL DEFAULT 0"),
    ("reasoning_tokens", "INTEGER NOT NULL DEFAULT 0"),
    ("ttft_ms", "INTEGER NOT NULL DEFAULT 0"),
    ("total_ms", "INTEGER NOT NULL DEFAULT 0"),
    ("circuit_breaker_tripped", "INTEGER NOT NULL DEFAULT 0"),
    ("cache_drop_breaker_tripped", "INTEGER NOT NULL DEFAULT 0"),
    ("compaction_triggered", "INTEGER NOT NULL DEFAULT 0"),
    ("compaction_tokens_reclaimed", "INTEGER NOT NULL DEFAULT 0"),
    ("ingress_tokens_saved", "INTEGER NOT NULL DEFAULT 0"),
    ("net_saved_usd", "REAL NOT NULL DEFAULT 0.0"),
];

/// Idempotently inspects `PRAGMA table_info(flight_recorder)` and runs `ALTER TABLE`
/// for any missing v3.1 columns so legacy v2/v3.0 databases upgrade without data loss.
pub fn migrate_flight_recorder_schema(conn: &Connection) -> Result<(), rusqlite::Error> {
    let mut stmt = conn.prepare("PRAGMA table_info(flight_recorder)")?;
    let existing: HashSet<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(Result::ok)
        .collect();
    for &(col, ddl) in MIGRATIONS {
        if !existing.contains(col) {
            let sql = format!("ALTER TABLE flight_recorder ADD COLUMN {col} {ddl}");
            conn.execute(&sql, [])?;
        }
    }
    Ok(())
}

pub const SELECT_COLS: &str = "timestamp_ms, session_id, turn_index, provider, model, status_code, \
    raw_tokens, opt_tokens, uncached_input_tokens, cache_read_tokens, cache_creation_tokens, \
    output_tokens, reasoning_tokens, ttft_ms, total_ms, c_eff_usd, slm_routed, \
    circuit_breaker_tripped, cache_drop_breaker_tripped, compaction_triggered, \
    compaction_tokens_reclaimed, ingress_tokens_saved, net_saved_usd, prompt_text, response_text";

pub fn map_audit_row(row: &Row<'_>) -> Result<AuditRecord, rusqlite::Error> {
    Ok(AuditRecord {
        timestamp_ms: row.get(0)?,
        session_id: row.get(1)?,
        turn_index: row.get::<_, i64>(2)? as usize,
        provider: row.get(3)?,
        model: row.get(4)?,
        status_code: row.get::<_, i64>(5)? as u16,
        raw_tokens: row.get::<_, i64>(6)? as usize,
        opt_tokens: row.get::<_, i64>(7)? as usize,
        uncached_input_tokens: row.get::<_, i64>(8)? as usize,
        cache_read_tokens: row.get::<_, i64>(9)? as usize,
        cache_creation_tokens: row.get::<_, i64>(10)? as usize,
        output_tokens: row.get::<_, i64>(11)? as usize,
        reasoning_tokens: row.get::<_, i64>(12)? as usize,
        ttft_ms: row.get::<_, i64>(13)? as u64,
        total_ms: row.get::<_, i64>(14)? as u64,
        c_eff_usd: row.get(15)?,
        slm_routed: row.get::<_, i64>(16)? != 0,
        circuit_breaker_tripped: row.get::<_, i64>(17)? != 0,
        cache_drop_breaker_tripped: row.get::<_, i64>(18)? != 0,
        compaction_triggered: row.get::<_, i64>(19)? != 0,
        compaction_tokens_reclaimed: row.get::<_, i64>(20)? as usize,
        ingress_tokens_saved: row.get::<_, i64>(21)? as usize,
        net_saved_usd: row.get(22)?,
        prompt_text: row.get(23)?,
        response_text: row.get(24)?,
    })
}

pub fn load_recent_tail(conn: &Connection, limit: usize) -> Result<Vec<AuditRecord>, rusqlite::Error> {
    let sql = format!("SELECT {SELECT_COLS} FROM flight_recorder ORDER BY id DESC LIMIT ?1");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([limit as i64], map_audit_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub fn apply_filter<'a>(records: &'a [AuditRecord], filter: &AuditFilter) -> Vec<&'a AuditRecord> {
    let prov = filter.provider.as_deref().map(normalize_provider_label);
    let model_sub = filter
        .model_substring
        .as_deref()
        .map(str::to_ascii_lowercase);
    records
        .iter()
        .filter(|r| {
            if let Some(ref p) = prov {
                if normalize_provider_label(&r.provider) != *p {
                    return false;
                }
            }
            if let Some(ref m) = model_sub {
                if !r.model.to_ascii_lowercase().contains(m.as_str()) {
                    return false;
                }
            }
            if let Some(since) = filter.since_timestamp_ms {
                if r.timestamp_ms < since {
                    return false;
                }
            }
            true
        })
        .collect()
}

pub fn summarize_records(records: &[&AuditRecord]) -> AuditSummary {
    let mut sum = AuditSummary::default();

    for r in records {
        sum.total_requests += 1;
        sum.total_raw_tokens += r.raw_tokens;
        sum.total_opt_tokens += r.opt_tokens;
        sum.total_uncached_input_tokens += r.uncached_input_tokens;
        sum.total_cache_read_tokens += r.cache_read_tokens;
        sum.total_cache_creation_tokens += r.cache_creation_tokens;
        sum.total_output_tokens += r.output_tokens;
        sum.total_reasoning_tokens += r.reasoning_tokens;
        if r.circuit_breaker_tripped {
            sum.total_circuit_breaker_trips += 1;
        }
        if r.cache_drop_breaker_tripped {
            sum.total_cache_drop_breaker_trips += 1;
        }
        if r.compaction_triggered {
            sum.total_compactions += 1;
            sum.total_compaction_reclaimed_tokens += r.compaction_tokens_reclaimed;
        }
        let rates = resolve_model_pricing(&r.provider, &r.model);
        if rates.input_per_m <= 0.0 && rates.output_per_m <= 0.0 {
            continue;
        }
        let uncached_rate = rates.input_per_m / 1_000_000.0;
        let cache_read_rate = rates.cache_read_mult * uncached_rate;
        let cache_write_rate = rates.cache_write_mult * uncached_rate;
        let output_rate = rates.output_per_m / 1_000_000.0;

        let actual_uncached = if r.uncached_input_tokens > 0 || r.cache_read_tokens > 0 {
            r.uncached_input_tokens
        } else {
            r.opt_tokens
        };
        let actual_turn_cost = (actual_uncached as f64) * uncached_rate
            + (r.cache_read_tokens as f64) * cache_read_rate
            + (r.cache_creation_tokens as f64) * cache_write_rate
            + (r.output_tokens as f64) * output_rate;
        sum.total_c_eff_usd += if r.c_eff_usd > 0.0 && r.uncached_input_tokens == 0 {
            r.c_eff_usd
        } else {
            actual_turn_cost
        };
        let (base_cached, base_uncached) = if r.turn_index > 1 {
            let c = ((r.raw_tokens as f64) * 0.90) as usize;
            (c, r.raw_tokens.saturating_sub(c))
        } else {
            (0, r.raw_tokens)
        };
        sum.asymmetric_baseline_usd += (base_uncached as f64) * uncached_rate
            + (base_cached as f64) * cache_read_rate
            + (r.output_tokens as f64) * output_rate;
    }
    sum
}

pub fn group_by_provider_model(records: &[&AuditRecord]) -> Vec<ProviderModelStats> {
    let mut map: BTreeMap<(String, String), ProviderModelStats> = BTreeMap::new();
    for r in records {
        let prov = normalize_provider_label(&r.provider);
        let model = if r.model.is_empty() {
            "unknown".to_owned()
        } else {
            r.model.clone()
        };
        let entry = map
            .entry((prov.clone(), model.clone()))
            .or_insert_with(|| ProviderModelStats {
                provider: prov,
                model,
                ..ProviderModelStats::default()
            });
        entry.requests += 1;
        entry.total_ttft_ms = entry.total_ttft_ms.saturating_add(r.ttft_ms);
        entry.cached_read_tokens += r.cache_read_tokens;
        entry.cache_write_tokens += r.cache_creation_tokens;
        entry.uncached_input_tokens += r.uncached_input_tokens;
        entry.output_tokens += r.output_tokens;
        entry.reasoning_tokens += r.reasoning_tokens;
        entry.ingress_tokens_saved += r
            .ingress_tokens_saved
            .max(r.raw_tokens.saturating_sub(r.opt_tokens));
        let net = compute_turn_net_saved_usd_for_model(
            &r.provider,
            &r.model,
            r.turn_index,
            r.raw_tokens,
            r.opt_tokens,
            r.uncached_input_tokens,
            r.cache_read_tokens,
            r.cache_creation_tokens,
            r.output_tokens,
            r.c_eff_usd,
        );
        entry.net_saved_usd += net;
    }
    let mut list: Vec<ProviderModelStats> = map.into_values().collect();
    list.sort_by_key(|a| std::cmp::Reverse(a.total_tokens()));
    list
}
