use aphelion_cli::proxy::{run_proxy_server, ProxyState};
use aphelion_telemetry::audit::AuditLedger;
use aphelion_telemetry::ledger::SessionCostSummary;
use std::env;
use std::fs::File;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::process::Command;

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let db_path = env::var("APHELION_AUDIT_DB").unwrap_or_else(|_| "aphelion_audit.db".to_owned());

    if let Some(first) = args.first().map(String::as_str) {
        match first {
            "--audit-summary" => return run_audit_summary(&db_path),
            "--audit-export" => {
                let Some(out_path) = args.get(1) else {
                    eprintln!("Usage: aphelion --audit-export <output.jsonl>");
                    return ExitCode::from(2);
                };
                return run_audit_export(&db_path, out_path);
            }
            "--help" | "-h" => {
                print_usage();
                return ExitCode::SUCCESS;
            }
            _ => {}
        }
    }

    let mut audit_enabled = false;
    let mut cmd_start = 0usize;
    while cmd_start < args.len() && args[cmd_start] == "--audit" {
        audit_enabled = true;
        cmd_start += 1;
    }

    if cmd_start >= args.len() {
        print_usage();
        return ExitCode::from(2);
    }

    let child_program = &args[cmd_start];
    let child_args = &args[cmd_start + 1..];

    let upstream_openai = env::var("OPENAI_BASE_URL")
        .unwrap_or_else(|_| "https://api.openai.com/v1".to_owned());
    let upstream_anthropic = env::var("ANTHROPIC_BASE_URL")
        .unwrap_or_else(|_| "https://api.anthropic.com".to_owned());

    let audit = if audit_enabled {
        match AuditLedger::open(&db_path) {
            Ok(db) => Some(db),
            Err(err) => {
                eprintln!("[aphelion] warning: failed to open audit db: {err}");
                None
            }
        }
    } else {
        None
    };

    let listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(l) => l,
        Err(err) => {
            eprintln!("[aphelion] failed to bind ephemeral proxy port: {err}");
            return ExitCode::from(1);
        }
    };
    let Ok(local_addr) = listener.local_addr() else {
        return ExitCode::from(1);
    };

    let state = Arc::new(ProxyState::new(
        upstream_openai,
        upstream_anthropic,
        audit.map(Mutex::new),
    ));
    let server_state = Arc::clone(&state);
    let server_task = tokio::spawn(async move {
        run_proxy_server(listener, server_state).await;
    });

    let proxy_base = format!("http://{local_addr}");
    let mut child = match Command::new(child_program)
        .args(child_args)
        .env("OPENAI_BASE_URL", format!("{proxy_base}/v1"))
        .env("ANTHROPIC_BASE_URL", &proxy_base)
        .spawn()
    {
        Ok(c) => c,
        Err(err) => {
            server_task.abort();
            eprintln!("[aphelion] failed to launch `{child_program}`: {err}");
            return ExitCode::from(1);
        }
    };

    let exit_code = tokio::select! {
        status = child.wait() => status.ok().and_then(|s| s.code()).unwrap_or(0),
        _ = tokio::signal::ctrl_c() => {
            let _ = child.kill().await;
            130
        }
    };

    server_task.abort();
    print_summary(&state.summary_snapshot());
    ExitCode::from(exit_code as u8)
}

fn run_audit_summary(db_path: &str) -> ExitCode {
    let Ok(ledger) = AuditLedger::open(db_path) else {
        eprintln!("[aphelion] no audit database found at {db_path}");
        return ExitCode::SUCCESS;
    };
    match ledger.summarize() {
        Ok(s) => {
            println!("| Metric | Value |");
            println!("| :--- | ---: |");
            println!("| Total Requests | {} |", s.total_requests);
            println!("| Verified Cache-Hit % | {:.1}% |", s.cache_hit_pct());
            println!("| BPE Tokens Saved | {} |", s.bpe_tokens_saved());
            println!("| Circuit-Breaker Trips | {} |", s.total_circuit_breaker_trips);
            println!("| Net Verified Saved | ${:.4} |", s.net_usd_saved());
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("[aphelion] failed reading audit summary: {err}");
            ExitCode::from(1)
        }
    }
}

fn run_audit_export(db_path: &str, out_path: &str) -> ExitCode {
    let Ok(ledger) = AuditLedger::open(db_path) else {
        eprintln!("[aphelion] failed opening audit db at {db_path}");
        return ExitCode::from(1);
    };
    let Ok(mut file) = File::create(out_path) else {
        eprintln!("[aphelion] failed creating {out_path}");
        return ExitCode::from(1);
    };
    match ledger.export_jsonl(&mut file) {
        Ok(n) => {
            println!("[aphelion] exported {n} trajectories to {out_path}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("[aphelion] export failed: {err}");
            ExitCode::from(1)
        }
    }
}

fn print_summary(summary: &SessionCostSummary) {
    if summary.turns == 0 {
        return;
    }
    eprintln!(
        "\n[aphelion] session complete | turns={} | raw_tok={} -> opt_tok={} ({:.1}% saved) | cache_read={} | C_eff=${:.4} vs baseline=${:.4} ({:.1}% saved)",
        summary.turns,
        summary.total_raw_tokens,
        summary.total_opt_tokens,
        summary.token_savings_pct(),
        summary.total_cache_read_tokens,
        summary.optimized_cost_usd,
        summary.baseline_cost_usd,
        summary.cost_savings_pct(),
    );
}

fn print_usage() {
    eprintln!("Usage:");
    eprintln!("  aphelion [--audit] <command> [args...]");
    eprintln!("  aphelion --audit-summary");
    eprintln!("  aphelion --audit-export <output.jsonl>");
}
