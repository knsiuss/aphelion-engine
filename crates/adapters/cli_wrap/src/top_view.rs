use crate::audit_report::{format_tokens, render_provider_model_table, truncate_str};
use aphelion_core::flight_recorder::{AuditFilter, ProviderModelStats};
use aphelion_telemetry::audit::{AuditLedger, AuditRecord, AuditSummary};
use std::env;
use std::fmt::Write as FmtWrite;
use std::io::{self, Write as IoWrite};
use std::process::ExitCode;
use std::time::Duration;

const ANSI_ENTER_ALT: &[u8] = b"\x1b[?1049h\x1b[?25l";
const ANSI_LEAVE_ALT: &[u8] = b"\x1b[?25h\x1b[?1049l";
const ANSI_HOME_CLEAR: &str = "\x1b[H\x1b[J";

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Self {
        let mut out = io::stdout().lock();
        let _ = out.write_all(ANSI_ENTER_ALT);
        let _ = out.flush();
        Self
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut out = io::stdout().lock();
        let _ = out.write_all(ANSI_LEAVE_ALT);
        let _ = out.flush();
    }
}

/// Formats UTC `HH:MM:SS` from a Unix epoch millisecond timestamp without external crates.
fn format_hms_utc(timestamp_ms: i64) -> String {
    let total_secs = timestamp_ms.max(0) / 1_000;
    let secs_of_day = total_secs % 86_400;
    let h = secs_of_day / 3_600;
    let m = (secs_of_day % 3_600) / 60;
    let s = secs_of_day % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

fn format_status(code: u16, use_color: bool) -> String {
    let status = if code == 0 { 200 } else { code };
    if !use_color {
        return format!("{status}");
    }
    let color = match status {
        200..=299 => "\x1b[32m",
        400..=499 => "\x1b[33m",
        _ => "\x1b[31m",
    };
    format!("{color}{status}\x1b[0m")
}

/// Pure, deterministic frame renderer for `aphelion top` (header + top-10 breakdown + 10-row tail).
pub fn build_top_frame(
    summary: &AuditSummary,
    groups: &[ProviderModelStats],
    tail: &[AuditRecord],
    interval_secs: u64,
    use_color: bool,
) -> String {
    let mut buf = String::with_capacity(2048);
    let _ = writeln!(
        buf,
        "APHELION LIVE MONITOR | Interval: {}s | Total Req: {} | Global Cache Hit: {:.1}% | Total Net Saved: ${:.4}",
        interval_secs,
        summary.total_requests,
        summary.cache_hit_pct(),
        summary.net_usd_saved()
    );
    let _ = writeln!(buf, "{}", "=".repeat(118));
    let _ = writeln!(buf, "USAGE BY PROVIDER & MODEL (Top 10)");
    buf.push_str(&render_provider_model_table(groups, Some(10)));
    let _ = writeln!(buf);
    let _ = writeln!(buf, "LIVE REQUEST TAIL (Last 10 Requests)");
    if tail.is_empty() {
        let _ = writeln!(buf, "  (waiting for requests — start `aphelion serve --audit` or `aphelion --audit <cmd>`)");
    } else {
        for r in tail.iter().take(10) {
            let hms = format_hms_utc(r.timestamp_ms);
            let prov_model = truncate_str(&format!("{}/{}", r.provider, r.model), 34);
            let status = format_status(r.status_code, use_color);
            let denom = r.cache_read_tokens + r.uncached_input_tokens + r.cache_creation_tokens;
            let hit_pct = if denom == 0 {
                0.0
            } else {
                (r.cache_read_tokens as f64 * 100.0) / (denom as f64)
            };
            let _ = writeln!(
                buf,
                "[{}] {:<34} | {} | {:>4}ms | hit {:>5.1}% | in:{}/{} | out:{}(r:{}) | ${:.4}",
                hms,
                prov_model,
                status,
                r.ttft_ms,
                hit_pct,
                format_tokens(r.cache_read_tokens),
                format_tokens(r.uncached_input_tokens),
                format_tokens(r.output_tokens),
                format_tokens(r.reasoning_tokens),
                r.net_saved_usd
            );
        }
    }
    let _ = writeln!(buf, "\nPress Ctrl+C to exit.");
    buf
}

fn parse_top_args(args: &[String], default_db: &str) -> (u64, String) {
    let mut interval = 1u64;
    let mut db = default_db.to_owned();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--interval" if i + 1 < args.len() => {
                interval = args[i + 1].parse::<u64>().unwrap_or(1).max(1);
                i += 2;
            }
            "--db" if i + 1 < args.len() => {
                db = args[i + 1].clone();
                i += 2;
            }
            _ => i += 1,
        }
    }
    (interval, db)
}

pub async fn run_top(args: &[String], default_db: &str) -> ExitCode {
    let (interval_secs, db_path) = parse_top_args(args, default_db);
    let use_color = env::var_os("NO_COLOR").is_none();
    let _guard = TerminalGuard::enter();
    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
    let filter = AuditFilter::default();

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = ticker.tick() => {
                let (summary, groups, tail) = match AuditLedger::open(&db_path) {
                    Ok(db) => (
                        db.summarize().unwrap_or_default(),
                        db.group_stats(&filter).unwrap_or_default(),
                        db.recent_tail(10).unwrap_or_default(),
                    ),
                    Err(_) => (AuditSummary::default(), Vec::new(), Vec::new()),
                };
                let frame = build_top_frame(&summary, &groups, &tail, interval_secs, use_color);
                let mut payload = String::with_capacity(frame.len() + ANSI_HOME_CLEAR.len());
                payload.push_str(ANSI_HOME_CLEAR);
                payload.push_str(&frame);
                let mut out = io::stdout().lock();
                if out.write_all(payload.as_bytes()).is_err() || out.flush().is_err() {
                    break;
                }
            }
        }
    }
    ExitCode::SUCCESS
}
