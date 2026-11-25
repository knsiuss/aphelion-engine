use aphelion_cli::audit_report::run_audit_summary_cmd;
use aphelion_cli::auth_cli::{run_auth_cmd, run_init_cmd};
use aphelion_cli::cli_audit::{print_summary, print_usage, run_audit_export};
use aphelion_cli::cli_commands::{run_doctor, run_ui_open};
use aphelion_cli::config::{GatewayConfig, DEFAULT_GATEWAY_PORT};
use aphelion_cli::provider_registry::build_upstream_targets;
use aphelion_cli::proxy::{run_proxy_server, ProxyState};
use aphelion_cli::spawn::build_child_command;
use aphelion_cli::top_view::run_top;
use aphelion_telemetry::audit::AuditLedger;
use std::env;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let db_path = GatewayConfig::env_db_path();

    match args.first().map(String::as_str) {
        Some("doctor") => {
            run_doctor();
            return ExitCode::SUCCESS;
        }
        Some("init") => return run_init_cmd(),
        Some("auth") => return run_auth_cmd(&args[1..]),
        Some("ui") => return run_ui_open(&args[1..]),
        Some("top") => return run_top(&args[1..], &db_path).await,
        Some("--audit-summary") => return run_audit_summary_cmd(&db_path, &args[1..]),
        Some("--audit-export") => {
            let Some(out) = args.get(1) else {
                eprintln!("Usage: aphelion --audit-export <output.jsonl>");
                return ExitCode::from(2);
            };
            return run_audit_export(&db_path, out);
        }
        Some("--help" | "-h") | None => {
            print_usage();
            return if args.is_empty() {
                ExitCode::from(2)
            } else {
                ExitCode::SUCCESS
            };
        }
        Some("serve") => return run_gateway_mode(&args[1..]).await,
        _ => {}
    }

    let cfg = GatewayConfig::from_env_and_args(&args, 0);
    if cfg.cmd_start >= args.len() {
        print_usage();
        return ExitCode::from(2);
    }

    let targets = build_upstream_targets(cfg.upstream.as_deref());
    let primary_upstream = targets.openai.clone();
    let audit = open_audit_if(cfg.audit, &cfg.db_path);
    let bind_addr = cfg.bind_addr();
    let Some((listener, local_addr)) = bind_addr_socket(&bind_addr).await else {
        return ExitCode::from(1);
    };

    if cfg.audit {
        eprintln!("[aphelion] listening on http://{local_addr} -> {primary_upstream}");
    }

    let state = Arc::new(ProxyState::with_targets(targets, audit.map(Mutex::new)));
    let server_task = tokio::spawn(run_proxy_server(listener, Arc::clone(&state)));

    let proxy_base = format!("http://{local_addr}");
    let mut cmd = build_child_command(
        &args[cfg.cmd_start],
        &args[cfg.cmd_start + 1..],
        &proxy_base,
    );
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(err) => {
            server_task.abort();
            eprintln!("[aphelion] failed to launch `{}`: {err}", args[cfg.cmd_start]);
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

async fn run_gateway_mode(args: &[String]) -> ExitCode {
    let cfg = GatewayConfig::from_env_and_args(args, DEFAULT_GATEWAY_PORT);
    let targets = build_upstream_targets(cfg.upstream.as_deref());
    let primary = targets.openai.clone();
    let audit = open_audit_if(cfg.audit, &cfg.db_path);
    let bind_addr = cfg.bind_addr();
    let Some((listener, local_addr)) = bind_addr_socket(&bind_addr).await else {
        return ExitCode::from(1);
    };

    let mode_tag = if cfg.public { "public" } else { "local" };
    eprintln!("[aphelion v3.8] listening on http://{local_addr} (mode={mode_tag}) -> {primary}");
    if !cfg.public {
        eprintln!("[aphelion v3.8] local dashboard: http://{local_addr}/ui");
    }

    let state = Arc::new(ProxyState::with_config(
        targets,
        cfg.public_config(),
        audit.map(Mutex::new),
    ));
    let server_task = tokio::spawn(run_proxy_server(listener, Arc::clone(&state)));
    let _ = tokio::signal::ctrl_c().await;
    server_task.abort();
    print_summary(&state.summary_snapshot());
    ExitCode::SUCCESS
}

async fn bind_addr_socket(addr: &str) -> Option<(TcpListener, std::net::SocketAddr)> {
    let listener = TcpListener::bind(addr).await.ok()?;
    let local = listener.local_addr().ok()?;
    Some((listener, local))
}

fn open_audit_if(enabled: bool, db_path: &str) -> Option<AuditLedger> {
    if !enabled {
        return None;
    }
    AuditLedger::open(db_path).ok()
}
