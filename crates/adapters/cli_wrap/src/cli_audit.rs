use aphelion_telemetry::audit::AuditLedger;
use aphelion_telemetry::ledger::SessionCostSummary;
use std::fs::File;
use std::process::ExitCode;

pub fn run_audit_summary(db_path: &str) -> ExitCode {
    let Ok(ledger) = AuditLedger::open(db_path) else {
        eprintln!("[aphelion] no audit database found at {db_path}");
        return ExitCode::SUCCESS;
    };
    match ledger.summarize() {
        Ok(s) => {
            println!("| Metric | Value |");
            println!("| :--- | ---: |");
            println!("| Total Requests | {} |", s.total_requests);
            println!(
                "| Uncached Input Tokens | {} |",
                s.total_uncached_input_tokens
            );
            println!("| Cached Read Tokens | {} |", s.total_cache_read_tokens);
            println!("| Cache Write Tokens | {} |", s.total_cache_creation_tokens);
            println!("| Output Tokens | {} |", s.total_output_tokens);
            println!("| Reasoning Tokens | {} |", s.total_reasoning_tokens);
            println!("| Verified Cache-Hit % | {:.1}% |", s.cache_hit_pct());
            println!(
                "| Compactions / Reclaimed | {} / {} tok |",
                s.total_compactions, s.total_compaction_reclaimed_tokens
            );
            println!(
                "| Circuit-Breaker Trips | {} |",
                s.total_circuit_breaker_trips
            );
            println!(
                "| Cache-Drop Breaker Trips | {} |",
                s.total_cache_drop_breaker_trips
            );
            println!("| BPE Tokens Saved (Ingress) | {} |", s.bpe_tokens_saved());
            println!("| True Net Verified Saved | ${:.4} |", s.net_usd_saved());
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("[aphelion] failed reading audit summary: {err}");
            ExitCode::from(1)
        }
    }
}

pub fn run_audit_export(db_path: &str, out_path: &str) -> ExitCode {
    let Ok(ledger) = AuditLedger::open(db_path) else {
        return ExitCode::from(1);
    };
    let Ok(mut file) = File::create(out_path) else {
        return ExitCode::from(1);
    };
    match ledger.export_jsonl(&mut file) {
        Ok(n) => {
            println!("[aphelion] exported {n} trajectories to {out_path}");
            ExitCode::SUCCESS
        }
        Err(_) => ExitCode::from(1),
    }
}

pub fn print_summary(summary: &SessionCostSummary) {
    if summary.turns == 0 {
        return;
    }
    eprintln!(
        "\n[aphelion] session complete | turns={} | raw_tok={} -> opt_tok={} ({:.1}% saved) | cache_read={} | C_eff=${:.4}",
        summary.turns,
        summary.total_raw_tokens,
        summary.total_opt_tokens,
        summary.token_savings_pct(),
        summary.total_cache_read_tokens,
        summary.optimized_cost_usd,
    );
}

pub fn print_usage() {
    eprintln!("Usage:");
    eprintln!("  aphelion [--audit] [--port <u16>] [--upstream <url>] <command> [args...]");
    eprintln!("  aphelion serve [--public] [--bind <addr>] [--port 18787] [--rpm 120] [--max-body-mb 10] [--audit]");
    eprintln!("  aphelion doctor");
    eprintln!("  aphelion --audit-summary | --audit-export <output.jsonl>");
}
