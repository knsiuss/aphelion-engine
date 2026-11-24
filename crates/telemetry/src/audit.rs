pub use crate::prefix_store::AuditSummary;
use crate::prefix_store::{init_prefix_schema, load_record, save_record};
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
    pub session_id: String,
    pub turn_index: usize,
    pub model: String,
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
    pub prompt_text: String,
    pub response_text: String,
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
                session_id TEXT NOT NULL,
                turn_index INTEGER NOT NULL,
                model TEXT NOT NULL,
                raw_tokens INTEGER NOT NULL,
                opt_tokens INTEGER NOT NULL,
                uncached_input_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL,
                cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                reasoning_tokens INTEGER NOT NULL DEFAULT 0,
                ttft_ms INTEGER NOT NULL DEFAULT 0,
                total_ms INTEGER NOT NULL DEFAULT 0,
                c_eff_usd REAL NOT NULL,
                slm_routed INTEGER NOT NULL,
                circuit_breaker_tripped INTEGER NOT NULL DEFAULT 0,
                cache_drop_breaker_tripped INTEGER NOT NULL DEFAULT 0,
                compaction_triggered INTEGER NOT NULL DEFAULT 0,
                compaction_tokens_reclaimed INTEGER NOT NULL DEFAULT 0,
                prompt_text TEXT NOT NULL,
                response_text TEXT NOT NULL,
                created_at TEXT DEFAULT CURRENT_TIMESTAMP
            );",
        )?;
        let _ = conn.execute("ALTER TABLE flight_recorder ADD COLUMN uncached_input_tokens INTEGER NOT NULL DEFAULT 0", []);
        let _ = conn.execute("ALTER TABLE flight_recorder ADD COLUMN cache_drop_breaker_tripped INTEGER NOT NULL DEFAULT 0", []);
        let _ = conn.execute("ALTER TABLE flight_recorder ADD COLUMN compaction_triggered INTEGER NOT NULL DEFAULT 0", []);
        let _ = conn.execute("ALTER TABLE flight_recorder ADD COLUMN compaction_tokens_reclaimed INTEGER NOT NULL DEFAULT 0", []);
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
        self.conn.execute(
            "INSERT INTO flight_recorder (
                session_id, turn_index, model, raw_tokens, opt_tokens,
                uncached_input_tokens, cache_read_tokens, cache_creation_tokens,
                output_tokens, reasoning_tokens, ttft_ms, total_ms, c_eff_usd,
                slm_routed, circuit_breaker_tripped, cache_drop_breaker_tripped,
                compaction_triggered, compaction_tokens_reclaimed,
                prompt_text, response_text
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
            params![
                rec.session_id, rec.turn_index as i64, rec.model, rec.raw_tokens as i64,
                rec.opt_tokens as i64, rec.uncached_input_tokens as i64, rec.cache_read_tokens as i64,
                rec.cache_creation_tokens as i64, rec.output_tokens as i64, rec.reasoning_tokens as i64,
                rec.ttft_ms as i64, rec.total_ms as i64, rec.c_eff_usd,
                i64::from(rec.slm_routed), i64::from(rec.circuit_breaker_tripped),
                i64::from(rec.cache_drop_breaker_tripped), i64::from(rec.compaction_triggered),
                rec.compaction_tokens_reclaimed as i64, rec.prompt_text, rec.response_text,
            ],
        )?;
        Ok(())
    }

    pub fn load_all(&self) -> Result<Vec<AuditRecord>, AuditError> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, turn_index, model, raw_tokens, opt_tokens,
                    uncached_input_tokens, cache_read_tokens, cache_creation_tokens,
                    output_tokens, reasoning_tokens, ttft_ms, total_ms, c_eff_usd,
                    slm_routed, circuit_breaker_tripped, cache_drop_breaker_tripped,
                    compaction_triggered, compaction_tokens_reclaimed,
                    prompt_text, response_text
             FROM flight_recorder ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(AuditRecord {
                session_id: row.get(0)?,
                turn_index: row.get::<_, i64>(1)? as usize,
                model: row.get(2)?,
                raw_tokens: row.get::<_, i64>(3)? as usize,
                opt_tokens: row.get::<_, i64>(4)? as usize,
                uncached_input_tokens: row.get::<_, i64>(5)? as usize,
                cache_read_tokens: row.get::<_, i64>(6)? as usize,
                cache_creation_tokens: row.get::<_, i64>(7)? as usize,
                output_tokens: row.get::<_, i64>(8)? as usize,
                reasoning_tokens: row.get::<_, i64>(9)? as usize,
                ttft_ms: row.get::<_, i64>(10)? as u64,
                total_ms: row.get::<_, i64>(11)? as u64,
                c_eff_usd: row.get(12)?,
                slm_routed: row.get::<_, i64>(13)? != 0,
                circuit_breaker_tripped: row.get::<_, i64>(14)? != 0,
                cache_drop_breaker_tripped: row.get::<_, i64>(15)? != 0,
                compaction_triggered: row.get::<_, i64>(16)? != 0,
                compaction_tokens_reclaimed: row.get::<_, i64>(17)? as usize,
                prompt_text: row.get(18)?,
                response_text: row.get(19)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn summarize(&self) -> Result<AuditSummary, AuditError> {
        let records = self.load_all()?;
        let mut sum = AuditSummary::default();
        let uncached_rate = 3.0 / 1_000_000.0;
        let cache_read_rate = 0.10 * uncached_rate;
        let cache_write_rate = 1.25 * uncached_rate;
        let output_rate = 15.0 / 1_000_000.0;

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

            // Asymmetric cached baseline: turns > 1 assume normal 90% upstream prefix cache hit.
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
        Ok(sum)
    }

    pub fn export_jsonl<W: Write>(&self, writer: &mut W) -> Result<usize, AuditError> {
        let records = self.load_all()?;
        let mut count = 0usize;
        for r in records {
            let trimmed = r.prompt_text.trim();
            if trimmed.starts_with('{') && trimmed.ends_with('}') {
                writeln!(writer, "{trimmed}")?;
                count += 1;
            }
        }
        Ok(count)
    }
}
