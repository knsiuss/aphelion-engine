use crate::audit_queries::{
    apply_filter, group_by_provider_model, load_recent_tail, map_audit_row,
    migrate_flight_recorder_schema, summarize_records, SELECT_COLS,
};
pub use crate::prefix_store::AuditSummary;
use crate::prefix_store::{init_prefix_schema, load_record, save_record};
use aphelion_core::flight_recorder::{
    compute_turn_net_saved_usd, normalize_provider_label, AuditFilter, ProviderModelStats,
};
use aphelion_core::prefix_ledger::{FrozenSessionRecord, LedgerKey};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::io::Write;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuditError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditRecord {
    pub timestamp_ms: i64,
    pub session_id: String,
    pub turn_index: usize,
    pub provider: String,
    pub model: String,
    pub status_code: u16,
    pub raw_tokens: usize,
    pub opt_tokens: usize,
    pub uncached_input_tokens: usize,
    pub cache_read_tokens: usize,
    pub cache_creation_tokens: usize,
    pub output_tokens: usize,
    pub reasoning_tokens: usize,
    pub ttft_ms: u64,
    pub total_ms: u64,
    pub c_eff_usd: f64,
    pub slm_routed: bool,
    pub circuit_breaker_tripped: bool,
    pub cache_drop_breaker_tripped: bool,
    pub compaction_triggered: bool,
    pub compaction_tokens_reclaimed: usize,
    pub ingress_tokens_saved: usize,
    pub net_saved_usd: f64,
    pub prompt_text: String,
    pub response_text: String,
}

#[derive(Serialize)]
struct ExportRow<'a> {
    timestamp_ms: i64,
    session_id: &'a str,
    turn_index: usize,
    provider: &'a str,
    model: &'a str,
    status_code: u16,
    ttft_ms: u64,
    total_latency_ms: u64,
    cached_read_tokens: usize,
    cache_write_tokens: usize,
    uncached_input_tokens: usize,
    output_tokens: usize,
    reasoning_tokens: usize,
    ingress_tokens_saved: usize,
    compaction_triggered: bool,
    net_saved_usd: f64,
}

pub struct AuditLedger {
    conn: Connection,
}

impl AuditLedger {
    pub fn open(path: &str) -> Result<Self, AuditError> {
        let conn = Connection::open(path)?;
        Self::init_schema(&conn)?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> Result<Self, AuditError> {
        let conn = Connection::open_in_memory()?;
        Self::init_schema(&conn)?;
        Ok(Self { conn })
    }

    fn init_schema(conn: &Connection) -> Result<(), AuditError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS flight_recorder (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp_ms INTEGER NOT NULL DEFAULT 0,
                session_id TEXT NOT NULL,
                turn_index INTEGER NOT NULL,
                provider TEXT NOT NULL DEFAULT 'unknown',
                model TEXT NOT NULL DEFAULT 'unknown',
                status_code INTEGER NOT NULL DEFAULT 200,
                raw_tokens INTEGER NOT NULL DEFAULT 0,
                opt_tokens INTEGER NOT NULL DEFAULT 0,
                uncached_input_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                reasoning_tokens INTEGER NOT NULL DEFAULT 0,
                ttft_ms INTEGER NOT NULL DEFAULT 0,
                total_ms INTEGER NOT NULL DEFAULT 0,
                c_eff_usd REAL NOT NULL DEFAULT 0.0,
                slm_routed INTEGER NOT NULL DEFAULT 0,
                circuit_breaker_tripped INTEGER NOT NULL DEFAULT 0,
                cache_drop_breaker_tripped INTEGER NOT NULL DEFAULT 0,
                compaction_triggered INTEGER NOT NULL DEFAULT 0,
                compaction_tokens_reclaimed INTEGER NOT NULL DEFAULT 0,
                ingress_tokens_saved INTEGER NOT NULL DEFAULT 0,
                net_saved_usd REAL NOT NULL DEFAULT 0.0,
                prompt_text TEXT NOT NULL DEFAULT '',
                response_text TEXT NOT NULL DEFAULT '',
                created_at TEXT DEFAULT CURRENT_TIMESTAMP
            );",
        )?;
        migrate_flight_recorder_schema(conn)?;
        init_prefix_schema(conn)?;
        Ok(())
    }

    pub fn save_prefix_record(
        &self,
        key: &LedgerKey,
        rec: &FrozenSessionRecord,
    ) -> Result<(), AuditError> {
        save_record(&self.conn, key, rec)?;
        Ok(())
    }

    pub fn load_prefix_record(&self, key: &LedgerKey) -> Option<FrozenSessionRecord> {
        load_record(&self.conn, key)
    }

    pub fn record(&self, rec: &AuditRecord) -> Result<(), AuditError> {
        let prov = normalize_provider_label(&rec.provider);
        let status = if rec.status_code == 0 { 200 } else { rec.status_code };
        let ingress_saved = if rec.ingress_tokens_saved > 0 {
            rec.ingress_tokens_saved
        } else {
            rec.raw_tokens.saturating_sub(rec.opt_tokens)
        };
        let net_saved = if rec.net_saved_usd != 0.0 {
            rec.net_saved_usd
        } else {
            compute_turn_net_saved_usd(
                rec.turn_index,
                rec.raw_tokens,
                rec.opt_tokens,
                rec.uncached_input_tokens,
                rec.cache_read_tokens,
                rec.cache_creation_tokens,
                rec.output_tokens,
                rec.c_eff_usd,
            )
        };
        self.conn.execute(
            "INSERT INTO flight_recorder (
                timestamp_ms, session_id, turn_index, provider, model, status_code,
                raw_tokens, opt_tokens, uncached_input_tokens, cache_read_tokens,
                cache_creation_tokens, output_tokens, reasoning_tokens, ttft_ms, total_ms,
                c_eff_usd, slm_routed, circuit_breaker_tripped, cache_drop_breaker_tripped,
                compaction_triggered, compaction_tokens_reclaimed, ingress_tokens_saved,
                net_saved_usd, prompt_text, response_text
            ) VALUES (
                ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25
            )",
            params![
                rec.timestamp_ms, rec.session_id, rec.turn_index as i64, prov, rec.model,
                i64::from(status), rec.raw_tokens as i64, rec.opt_tokens as i64,
                rec.uncached_input_tokens as i64, rec.cache_read_tokens as i64,
                rec.cache_creation_tokens as i64, rec.output_tokens as i64,
                rec.reasoning_tokens as i64, rec.ttft_ms as i64, rec.total_ms as i64,
                rec.c_eff_usd, i64::from(rec.slm_routed), i64::from(rec.circuit_breaker_tripped),
                i64::from(rec.cache_drop_breaker_tripped), i64::from(rec.compaction_triggered),
                rec.compaction_tokens_reclaimed as i64, ingress_saved as i64, net_saved,
                rec.prompt_text, rec.response_text,
            ],
        )?;
        Ok(())
    }

    pub fn load_all(&self) -> Result<Vec<AuditRecord>, AuditError> {
        let sql = format!("SELECT {SELECT_COLS} FROM flight_recorder ORDER BY id ASC");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_audit_row)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn recent_tail(&self, limit: usize) -> Result<Vec<AuditRecord>, AuditError> {
        Ok(load_recent_tail(&self.conn, limit)?)
    }

    pub fn summarize(&self) -> Result<AuditSummary, AuditError> {
        self.summarize_filtered(&AuditFilter::default())
    }

    pub fn summarize_filtered(&self, filter: &AuditFilter) -> Result<AuditSummary, AuditError> {
        let all = self.load_all()?;
        let filtered = apply_filter(&all, filter);
        Ok(summarize_records(&filtered))
    }

    pub fn group_stats(&self, filter: &AuditFilter) -> Result<Vec<ProviderModelStats>, AuditError> {
        let all = self.load_all()?;
        let filtered = apply_filter(&all, filter);
        Ok(group_by_provider_model(&filtered))
    }

    pub fn export_jsonl<W: Write>(&self, writer: &mut W) -> Result<usize, AuditError> {
        let records = self.load_all()?;
        let mut count = 0usize;
        for r in &records {
            let prov = normalize_provider_label(&r.provider);
            let row = ExportRow {
                timestamp_ms: r.timestamp_ms,
                session_id: &r.session_id,
                turn_index: r.turn_index,
                provider: &prov,
                model: &r.model,
                status_code: r.status_code,
                ttft_ms: r.ttft_ms,
                total_latency_ms: r.total_ms,
                cached_read_tokens: r.cache_read_tokens,
                cache_write_tokens: r.cache_creation_tokens,
                uncached_input_tokens: r.uncached_input_tokens,
                output_tokens: r.output_tokens,
                reasoning_tokens: r.reasoning_tokens,
                ingress_tokens_saved: r.ingress_tokens_saved,
                compaction_triggered: r.compaction_triggered,
                net_saved_usd: r.net_saved_usd,
            };
            if let Ok(line) = serde_json::to_string(&row) {
                writeln!(writer, "{line}")?;
                count += 1;
            }
        }
        Ok(count)
    }
}
