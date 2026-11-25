use aphelion_core::flight_recorder::{
    canonical_provider, parse_duration_ms, AuditFilter, ProviderModelStats,
};
use aphelion_telemetry::audit::{AuditError, AuditLedger, AuditSummary};
use std::fmt::Write;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

/// Formats token counts compactly (`840`, `182.4k`, `1.2M`).
pub fn format_tokens(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// UTF-8 safe string truncation with ASCII ellipsis.
pub fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_owned();
    }
    if max_chars <= 3 {
        return s.chars().take(max_chars).collect();
    }
    let prefix: String = s.chars().take(max_chars - 3).collect();
    format!("{prefix}...")
}

pub fn parse_audit_filter(args: &[String]) -> AuditFilter {
    let mut filter = AuditFilter::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--provider" if i + 1 < args.len() => {
                filter.provider = Some(canonical_provider(&args[i + 1]).to_owned());
                i += 2;
            }
            "--model" if i + 1 < args.len() => {
                filter.model_substring = Some(args[i + 1].clone());
                i += 2;
            }
            "--last" if i + 1 < args.len() => {
                if let Some(dur_ms) = parse_duration_ms(&args[i + 1]) {
                    let now_ms = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);
                    filter.since_timestamp_ms = Some(now_ms.saturating_sub(dur_ms));
                }
                i += 2;
            }
            _ => i += 1,
        }
    }
    filter
}

pub fn render_global_summary(s: &AuditSummary) -> String {
    let mut out = String::with_capacity(512);
    let _ = writeln!(out, "| Metric | Value |");
    let _ = writeln!(out, "| :--- | ---: |");
    let _ = writeln!(out, "| Total Requests | {} |", s.total_requests);
    let _ = writeln!(
        out,
        "| Uncached Input Tokens | {} |",
        s.total_uncached_input_tokens
    );
    let _ = writeln!(out, "| Cached Read Tokens | {} |", s.total_cache_read_tokens);
    let _ = writeln!(
        out,
        "| Cache Write Tokens | {} |",
        s.total_cache_creation_tokens
    );
    let _ = writeln!(out, "| Output Tokens | {} |", s.total_output_tokens);
    let _ = writeln!(out, "| Reasoning Tokens | {} |", s.total_reasoning_tokens);
    let _ = writeln!(out, "| Verified Cache-Hit % | {:.1}% |", s.cache_hit_pct());
    let _ = writeln!(
        out,
        "| Compactions / Reclaimed | {} / {} tok |",
        s.total_compactions, s.total_compaction_reclaimed_tokens
    );
    let _ = writeln!(
        out,
        "| Circuit-Breaker Trips | {} |",
        s.total_circuit_breaker_trips
    );
    let _ = writeln!(
        out,
        "| Cache-Drop Breaker Trips | {} |",
        s.total_cache_drop_breaker_trips
    );
    let _ = writeln!(
        out,
        "| BPE Tokens Saved (Ingress) | {} |",
        s.bpe_tokens_saved()
    );
    let _ = writeln!(out, "| True Net Verified Saved | ${:.4} |", s.net_usd_saved());
    out
}

pub fn render_provider_model_table(
    groups: &[ProviderModelStats],
    max_rows: Option<usize>,
) -> String {
    let mut out = String::with_capacity(512);
    let _ = writeln!(
        out,
        "| {:<11} | {:<26} | {:>4} | {:>8} | {:>11} | {:>20} | {:>15} | {:>9} |",
        "Provider",
        "Model",
        "Req",
        "Avg TTFT",
        "Cache Hit %",
        "In (Cached/Uncached)",
        "Out (Reason)",
        "Net Saved"
    );
    let _ = writeln!(
        out,
        "| {:<11} | {:<26} | {:>4} | {:>8} | {:>11} | {:>20} | {:>15} | {:>9} |",
        ":---", ":---", "---:", "---:", "---:", "---:", "---:", "---:"
    );
    let limit = max_rows.unwrap_or(groups.len());
    if groups.is_empty() {
        let _ = writeln!(out, "| (no data)   | -                          |    0 |      0ms |        0.0% |                  0/0 |          0 (0)  |   $0.0000 |");
        return out;
    }
    for g in groups.iter().take(limit) {
        let prov = truncate_str(&g.provider, 11);
        let model = truncate_str(&g.model, 26);
        let ttft = format!("{}ms", g.avg_ttft_ms());
        let hit = format!("{:.1}%", g.cache_hit_pct());
        let in_tok = format!(
            "{}/{}",
            format_tokens(g.cached_read_tokens),
            format_tokens(g.uncached_input_tokens)
        );
        let out_tok = format!(
            "{} ({})",
            format_tokens(g.output_tokens),
            format_tokens(g.reasoning_tokens)
        );
        let saved = format!("${:.4}", g.net_saved_usd);
        let _ = writeln!(
            out,
            "| {:<11} | {:<26} | {:>4} | {:>8} | {:>11} | {:>20} | {:>15} | {:>9} |",
            prov, model, g.requests, ttft, hit, in_tok, out_tok, saved
        );
    }
    out
}

pub fn render_audit_report(
    ledger: &AuditLedger,
    filter: &AuditFilter,
) -> Result<String, AuditError> {
    let summary = ledger.summarize_filtered(filter)?;
    let groups = ledger.group_stats(filter)?;
    let mut out = render_global_summary(&summary);
    out.push('\n');
    out.push_str(&render_provider_model_table(&groups, None));
    Ok(out)
}

pub fn run_audit_summary_cmd(db_path: &str, args: &[String]) -> ExitCode {
    let Ok(ledger) = AuditLedger::open(db_path) else {
        eprintln!("[aphelion] no audit database found at {db_path}");
        return ExitCode::SUCCESS;
    };
    let filter = parse_audit_filter(args);
    match render_audit_report(&ledger, &filter) {
        Ok(report) => {
            print!("{report}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("[aphelion] failed reading audit summary: {err}");
            ExitCode::from(1)
        }
    }
}
