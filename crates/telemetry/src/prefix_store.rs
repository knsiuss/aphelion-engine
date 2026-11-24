use aphelion_core::prefix_ledger::{FrozenSessionRecord, LedgerKey};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditSummary {
    pub total_requests: usize,
    pub total_raw_tokens: usize,
    pub total_opt_tokens: usize,
    pub total_uncached_input_tokens: usize,
    pub total_cache_read_tokens: usize,
    pub total_cache_creation_tokens: usize,
    pub total_output_tokens: usize,
    pub total_circuit_breaker_trips: usize,
    pub total_cache_drop_breaker_trips: usize,
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
    /// Returns negative values if cache-miss regressions caused actual cost to exceed baseline.
    pub fn net_usd_saved(&self) -> f64 {
        self.asymmetric_baseline_usd - self.total_c_eff_usd
    }
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

pub fn save_record(
    conn: &Connection,
    key: &LedgerKey,
    record: &FrozenSessionRecord,
) -> Result<(), rusqlite::Error> {
    let json = serde_json::to_string(record).unwrap_or_else(|_| "{}".to_owned());
    let hash_hex = format!("{:032x}", key.session_root_hash);
    conn.execute(
        "INSERT INTO prefix_ledger (session_root_hash, model_id, committed_len, record_json)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(session_root_hash, model_id) DO UPDATE SET
            committed_len = excluded.committed_len,
            record_json = excluded.record_json,
            updated_at = CURRENT_TIMESTAMP",
        params![hash_hex, key.model_id, record.committed_len as i64, json],
    )?;
    Ok(())
}

pub fn load_record(conn: &Connection, key: &LedgerKey) -> Option<FrozenSessionRecord> {
    let hash_hex = format!("{:032x}", key.session_root_hash);
    let mut stmt = conn
        .prepare(
            "SELECT record_json FROM prefix_ledger WHERE session_root_hash = ?1 AND model_id = ?2",
        )
        .ok()?;
    let json_str: String = stmt
        .query_row(params![hash_hex, key.model_id], |row| row.get(0))
        .ok()?;
    serde_json::from_str(&json_str).ok()
}
