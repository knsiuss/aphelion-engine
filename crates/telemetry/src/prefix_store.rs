use aphelion_core::prefix_ledger::{FrozenSessionRecord, LedgerKey};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditSummary {
    pub total_requests: usize,
    pub total_raw_tokens: usize,
    pub total_opt_tokens: usize,
    pub total_uncached_input_tokens: usize,
    pub total_cache_read_tokens: usize,
    pub total_cache_creation_tokens: usize,
    pub total_output_tokens: usize,
    pub total_reasoning_tokens: usize,
    pub total_circuit_breaker_trips: usize,
    pub total_cache_drop_breaker_trips: usize,
    pub total_compactions: usize,
    pub total_compaction_reclaimed_tokens: usize,
    pub total_c_eff_usd: f64,
    pub asymmetric_baseline_usd: f64,
}

impl AuditSummary {
    pub fn cache_hit_pct(&self) -> f64 {
        let total_in = self.total_uncached_input_tokens
            + self.total_cache_read_tokens
            + self.total_cache_creation_tokens;
        let denom = if total_in > 0 {
            total_in
        } else {
            self.total_opt_tokens
        };
        if denom == 0 {
            return 0.0;
        }
        (self.total_cache_read_tokens as f64 * 100.0) / (denom as f64)
    }

    pub fn bpe_tokens_saved(&self) -> usize {
        self.total_raw_tokens.saturating_sub(self.total_opt_tokens)
    }

    /// True Net Verified Saved ($) using asymmetric cached vs. uncached pricing.
    pub fn net_usd_saved(&self) -> f64 {
        self.asymmetric_baseline_usd - self.total_c_eff_usd
    }
}

/// Zero-Retention persisted metadata for `prefix_ledger`: stores ONLY cryptographic
/// file hashes (`XXH3_64`), lengths, and rolling cache counters — never raw prompt or file text.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PersistedPrefixMeta {
    frozen_file_hashes: HashMap<String, u64>,
    committed_len: usize,
    compaction_epoch: u32,
    #[serde(default)]
    compacted_dropped_count: usize,
    rolling_cached_read: usize,
    rolling_total_input: usize,
    usage_turns: usize,
    upstream_supports_cache: bool,
    cache_drop_breaker_tripped: bool,
}

pub fn init_prefix_schema(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS prefix_ledger (
            session_root_hash TEXT NOT NULL,
            model_id TEXT NOT NULL,
            committed_len INTEGER NOT NULL,
            record_json TEXT NOT NULL,
            updated_at TEXT DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (session_root_hash, model_id)
        );",
    )
}

fn composite_session_key(key: &LedgerKey) -> String {
    format!(
        "{:016x}:{:032x}:{}",
        key.tenant_hash, key.session_root_hash, key.provider_id
    )
}

pub fn save_record(
    conn: &Connection,
    key: &LedgerKey,
    record: &FrozenSessionRecord,
) -> Result<(), rusqlite::Error> {
    let meta = PersistedPrefixMeta {
        frozen_file_hashes: record.frozen_file_hashes.clone(),
        committed_len: record.committed_len,
        compaction_epoch: record.compaction_epoch,
        compacted_dropped_count: record.compacted_dropped_count,
        rolling_cached_read: record.rolling_cached_read,
        rolling_total_input: record.rolling_total_input,
        usage_turns: record.usage_turns,
        upstream_supports_cache: record.upstream_supports_cache,
        cache_drop_breaker_tripped: record.cache_drop_breaker_tripped,
    };
    let json = serde_json::to_string(&meta).unwrap_or_else(|_| "{}".to_owned());
    let skey = composite_session_key(key);
    conn.execute(
        "INSERT INTO prefix_ledger (session_root_hash, model_id, committed_len, record_json)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(session_root_hash, model_id) DO UPDATE SET
            committed_len = excluded.committed_len,
            record_json = excluded.record_json,
            updated_at = CURRENT_TIMESTAMP",
        params![skey, key.model_id, record.committed_len as i64, json],
    )?;
    Ok(())
}

pub fn load_record(conn: &Connection, key: &LedgerKey) -> Option<FrozenSessionRecord> {
    let skey = composite_session_key(key);
    let mut stmt = conn
        .prepare(
            "SELECT record_json FROM prefix_ledger WHERE session_root_hash = ?1 AND model_id = ?2",
        )
        .ok()?;
    let json_str: String = stmt
        .query_row(params![skey, key.model_id], |row| row.get(0))
        .ok()?;
    let meta: PersistedPrefixMeta = serde_json::from_str(&json_str).ok()?;
    Some(FrozenSessionRecord {
        frozen_prefix_bytes: Vec::new(),
        frozen_messages: Vec::new(),
        frozen_file_hashes: meta.frozen_file_hashes,
        committed_len: meta.committed_len,
        compaction_epoch: meta.compaction_epoch,
        compacted_dropped_count: meta.compacted_dropped_count,
        rolling_cached_read: meta.rolling_cached_read,
        rolling_total_input: meta.rolling_total_input,
        usage_turns: meta.usage_turns,
        upstream_supports_cache: meta.upstream_supports_cache,
        cache_drop_breaker_tripped: meta.cache_drop_breaker_tripped,
    })
}
