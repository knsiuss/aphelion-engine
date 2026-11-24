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
    pub cache_read_tokens: usize,
    pub cache_creation_tokens: usize,
    pub output_tokens: usize,
    pub reasoning_tokens: usize,
    pub ttft_ms: u64,
    pub total_ms: u64,
    pub c_eff_usd: f64,
    pub slm_routed: bool,
    pub circuit_breaker_tripped: bool,
    pub prompt_text: String,
    pub response_text: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditSummary {
    pub total_requests: usize,
    pub total_raw_tokens: usize,
    pub total_opt_tokens: usize,
    pub total_cache_read_tokens: usize,
    pub total_circuit_breaker_trips: usize,
    pub total_c_eff_usd: f64,
    pub estimated_baseline_usd: f64,
}

impl AuditSummary {
    pub fn cache_hit_pct(&self) -> f64 {
        if self.total_opt_tokens == 0 {
            return 0.0;
        }
        (self.total_cache_read_tokens as f64 * 100.0) / (self.total_opt_tokens as f64)
    }

    pub fn bpe_tokens_saved(&self) -> usize {
        self.total_raw_tokens.saturating_sub(self.total_opt_tokens)
    }

    pub fn net_usd_saved(&self) -> f64 {
        (self.estimated_baseline_usd - self.total_c_eff_usd).max(0.0)
    }
}

/// SQLite Flight Recorder storing turn-by-turn proxy metrics, verified upstream usage,
/// and sanitized trajectories for `--audit-summary`, `--audit-export`, and distillation.
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
                cache_read_tokens INTEGER NOT NULL,
                cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                reasoning_tokens INTEGER NOT NULL DEFAULT 0,
                ttft_ms INTEGER NOT NULL DEFAULT 0,
                total_ms INTEGER NOT NULL DEFAULT 0,
                c_eff_usd REAL NOT NULL,
                slm_routed INTEGER NOT NULL,
                circuit_breaker_tripped INTEGER NOT NULL DEFAULT 0,
                prompt_text TEXT NOT NULL,
                response_text TEXT NOT NULL,
                created_at TEXT DEFAULT CURRENT_TIMESTAMP
            );",
        )?;
        Ok(())
    }

    pub fn record(&self, rec: &AuditRecord) -> Result<(), AuditError> {
        self.conn.execute(
            "INSERT INTO flight_recorder (
                session_id, turn_index, model, raw_tokens, opt_tokens,
                cache_read_tokens, cache_creation_tokens, output_tokens, reasoning_tokens,
                ttft_ms, total_ms, c_eff_usd, slm_routed, circuit_breaker_tripped,
                prompt_text, response_text
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                rec.session_id,
                rec.turn_index as i64,
                rec.model,
                rec.raw_tokens as i64,
                rec.opt_tokens as i64,
                rec.cache_read_tokens as i64,
                rec.cache_creation_tokens as i64,
                rec.output_tokens as i64,
                rec.reasoning_tokens as i64,
                rec.ttft_ms as i64,
                rec.total_ms as i64,
                rec.c_eff_usd,
                i64::from(rec.slm_routed),
                i64::from(rec.circuit_breaker_tripped),
                rec.prompt_text,
                rec.response_text,
            ],
        )?;
        Ok(())
    }

    pub fn load_all(&self) -> Result<Vec<AuditRecord>, AuditError> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, turn_index, model, raw_tokens, opt_tokens,
                    cache_read_tokens, cache_creation_tokens, output_tokens, reasoning_tokens,
                    ttft_ms, total_ms, c_eff_usd, slm_routed, circuit_breaker_tripped,
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
                cache_read_tokens: row.get::<_, i64>(5)? as usize,
                cache_creation_tokens: row.get::<_, i64>(6)? as usize,
                output_tokens: row.get::<_, i64>(7)? as usize,
                reasoning_tokens: row.get::<_, i64>(8)? as usize,
                ttft_ms: row.get::<_, i64>(9)? as u64,
                total_ms: row.get::<_, i64>(10)? as u64,
                c_eff_usd: row.get(11)?,
                slm_routed: row.get::<_, i64>(12)? != 0,
                circuit_breaker_tripped: row.get::<_, i64>(13)? != 0,
                prompt_text: row.get(14)?,
                response_text: row.get(15)?,
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
        for r in records {
            sum.total_requests += 1;
            sum.total_raw_tokens += r.raw_tokens;
            sum.total_opt_tokens += r.opt_tokens;
            sum.total_cache_read_tokens += r.cache_read_tokens;
            if r.circuit_breaker_tripped {
                sum.total_circuit_breaker_trips += 1;
            }
            sum.total_c_eff_usd += r.c_eff_usd;
            sum.estimated_baseline_usd +=
                (r.raw_tokens as f64) * 3.0 / 1_000_000.0 + (r.output_tokens as f64) * 15.0 / 1_000_000.0;
        }
        Ok(sum)
    }

    /// Exports sanitized JSON request payloads as `.jsonl` ready for `aphelion-bench-harness`.
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
