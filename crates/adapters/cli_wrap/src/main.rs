use aphelion_cli::headers::UpstreamTargets;
use aphelion_cli::proxy::{run_proxy_server, ProxyState};
use aphelion_cli::spawn::build_child_command;
use aphelion_telemetry::audit::AuditLedger;
use aphelion_telemetry::ledger::SessionCostSummary;
use std::env;
use std::fs::File;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let db_path = env::var("APHELION_AUDIT_DB").unwrap_or_else(|_| "aphelion_audit.db".to_owned());

    match args.first().map(String::as_str) {
        Some("--audit-summary") => return run_audit_summary(&db_path),
        Some("--audit-export") => {
            let Some(out) = args.get(1) else {
                eprintln!("Usage: aphelion --audit-export <output.jsonl>");
                return ExitCode::from(2);
            };
            return run_audit_export(&db_path, out);
        }
        Some("--help" | "-h") | None => {
            print_usage();
            return if args.is_empty() { ExitCode::from(2) } else { ExitCode::SUCCESS };
        }
        Some("serve") => return run_gateway_mode(&args[1..], &db_path).await,
        _ => {}
    }

    let (audit_enabled, port, upstream_override, cmd_start) = parse_flags(&args, 0);
    if cmd_start >= args.len() {
        print_usage();
        return ExitCode::from(2);
    }

    let targets = build_upstream_targets(upstream_override.as_deref());
    let primary_upstream = targets.openai.clone();
    let audit = open_audit_if(audit_enabled, &db_path);
    let (listener, local_addr) = match bind_port(port).await {
        Some(pair) => pair,
        None => return ExitCode::from(1),
    };

    if audit_enabled {
        eprintln!("[aphelion] listening on http://{local_addr} -> {primary_upstream}");
    }

    let state = Arc::new(ProxyState::with_targets(targets, audit.map(Mutex::new)));
    let server_task = tokio::spawn(run_proxy_server(listener, Arc::clone(&state)));

    let proxy_base = format!("http://{local_addr}");
    let mut cmd = build_child_command(&args[cmd_start], &args[cmd_start + 1..], &proxy_base);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(err) => {
            server_task.abort();
            eprintln!("[aphelion] failed to launch `{}`: {err}", args[cmd_start]);
            return ExitCode::from(1);
        }
    };

    let exit_code = tokio::select! {
        status = child.wait() => status.ok().and_then(|s| s.code()).unwrap_or(0),
        _ = tokio::signal::ctrl_c() => { let _ = child.kill().await; 130 }
    };

    server_task.abort();
    print_summary(&state.summary_snapshot());
    ExitCode::from(exit_code as u8)
}

async fn run_gateway_mode(args: &[String], db_path: &str) -> ExitCode {
    let (audit_enabled, port, upstream_override, _) = parse_flags(args, 18787);
    let targets = build_upstream_targets(upstream_override.as_deref());
    let primary = targets.openai.clone();
    let audit = open_audit_if(audit_enabled, db_path);
    let (listener, local_addr) = match bind_port(port).await {
        Some(pair) => pair,
        None => return ExitCode::from(1),
    };

    eprintln!("[aphelion] listening on http://{local_addr} -> {primary}");
    let state = Arc::new(ProxyState::with_targets(targets, audit.map(Mutex::new)));
    let server_task = tokio::spawn(run_proxy_server(listener, Arc::clone(&state)));
    let _ = tokio::signal::ctrl_c().await;
    server_task.abort();
    print_summary(&state.summary_snapshot());
    ExitCode::SUCCESS
}

fn parse_flags(args: &[String], default_port: u16) -> (bool, u16, Option<String>, usize) {
    let mut audit = false;
    let mut port = default_port;
    let mut upstream = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--audit" => {
                audit = true;
                i += 1;
            }
            "--port" if i + 1 < args.len() => {
                port = args[i + 1].parse().unwrap_or(default_port);
                i += 2;
            }
            "--upstream" if i + 1 < args.len() => {
                upstream = Some(args[i + 1].clone());
                i += 2;
            }
            _ => break,
        }
    }
    (audit, port, upstream, i)
}

fn build_upstream_targets(override_url: Option<&str>) -> UpstreamTargets {
    let openai = override_url
        .map(str::to_owned)
        .or_else(|| env::var("OPENAI_BASE_URL").ok())
        .unwrap_or_else(|| "https://api.openai.com/v1".to_owned());
    let anthropic = override_url
        .map(str::to_owned)
        .or_else(|| env::var("ANTHROPIC_BASE_URL").ok())
        .unwrap_or_else(|| "https://api.anthropic.com".to_owned());
    let gemini = env::var("GEMINI_API_BASE_URL")
        .or_else(|_| env::var("GOOGLE_GEMINI_BASE_URL"))
        .unwrap_or_else(|_| "https://generativelanguage.googleapis.com".to_owned());
    let ollama = env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://127.0.0.1:11434".to_owned());
    UpstreamTargets { openai, anthropic, gemini, ollama }
}

async fn bind_port(port: u16) -> Option<(TcpListener, std::net::SocketAddr)> {
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).await.ok()?;
    let addr = listener.local_addr().ok()?;
    Some((listener, addr))
}

fn open_audit_if(enabled: bool, db_path: &str) -> Option<AuditLedger> {
    if !enabled {
        return None;
    }
    AuditLedger::open(db_path).ok()
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
    let Ok(ledger) = AuditLedger::open(db_path) else { return ExitCode::from(1) };
    let Ok(mut file) = File::create(out_path) else { return ExitCode::from(1) };
    match ledger.export_jsonl(&mut file) {
        Ok(n) => { println!("[aphelion] exported {n} trajectories to {out_path}"); ExitCode::SUCCESS }
        Err(_) => ExitCode::from(1),
    }
}

fn print_summary(summary: &SessionCostSummary) {
    if summary.turns == 0 { return; }
    eprintln!(
        "\n[aphelion] session complete | turns={} | raw_tok={} -> opt_tok={} ({:.1}% saved) | cache_read={} | C_eff=${:.4} vs baseline=${:.4} ({:.1}% saved)",
        summary.turns, summary.total_raw_tokens, summary.total_opt_tokens, summary.token_savings_pct(),
        summary.total_cache_read_tokens, summary.optimized_cost_usd, summary.baseline_cost_usd, summary.cost_savings_pct(),
    );
}

fn print_usage() {
    eprintln!("Usage:");
    eprintln!("  aphelion [--audit] [--port <u16>] [--upstream <url>] <command> [args...]");
    eprintln!("  aphelion serve [--port 18787] [--upstream <url>] [--audit]");
    eprintln!("  aphelion --audit-summary");
    eprintln!("  aphelion --audit-export <output.jsonl>");
}
