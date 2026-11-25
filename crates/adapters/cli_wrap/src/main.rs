use aphelion_cli::audit_report::run_audit_summary_cmd;
use aphelion_cli::cli_audit::{print_summary, print_usage, run_audit_export};
use aphelion_cli::cli_commands::{run_doctor, run_ui_open};
use aphelion_cli::headers::UpstreamTargets;
use aphelion_cli::proxy::{run_proxy_server, ProxyState};
use aphelion_cli::public_guard::PublicConfig;
use aphelion_cli::spawn::build_child_command;
use aphelion_cli::top_view::run_top;
use aphelion_telemetry::audit::AuditLedger;
use std::env;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

struct CliFlags {
    audit: bool,
    public: bool,
    admin_token: Option<String>,
    port: u16,
    bind: Option<String>,
    rpm: u32,
    max_body_mb: usize,
    upstream: Option<String>,
    cmd_start: usize,
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let db_path = env::var("APHELION_AUDIT_DB").unwrap_or_else(|_| "aphelion_audit.db".to_owned());

    match args.first().map(String::as_str) {
        Some("doctor") => {
            run_doctor();
            return ExitCode::SUCCESS;
        }
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
        Some("serve") => return run_gateway_mode(&args[1..], &db_path).await,
        _ => {}
    }

    let flags = parse_flags(&args, 0);
    if flags.cmd_start >= args.len() {
        print_usage();
        return ExitCode::from(2);
    }

    let targets = build_upstream_targets(flags.upstream.as_deref());
    let primary_upstream = targets.openai.clone();
    let audit = open_audit_if(flags.audit, &db_path);
    let bind_addr = flags
        .bind
        .unwrap_or_else(|| format!("127.0.0.1:{}", flags.port));
    let Some((listener, local_addr)) = bind_addr_socket(&bind_addr).await else {
        return ExitCode::from(1);
    };

    if flags.audit {
        eprintln!("[aphelion] listening on http://{local_addr} -> {primary_upstream}");
    }

    let state = Arc::new(ProxyState::with_targets(targets, audit.map(Mutex::new)));
    let server_task = tokio::spawn(run_proxy_server(listener, Arc::clone(&state)));

    let proxy_base = format!("http://{local_addr}");
    let mut cmd = build_child_command(
        &args[flags.cmd_start],
        &args[flags.cmd_start + 1..],
        &proxy_base,
    );
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(err) => {
            server_task.abort();
            eprintln!(
                "[aphelion] failed to launch `{}`: {err}",
                args[flags.cmd_start]
            );
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
    let flags = parse_flags(args, 18787);
    let targets = build_upstream_targets(flags.upstream.as_deref());
    let primary = targets.openai.clone();
    let audit = open_audit_if(flags.audit, db_path);
    let default_host = if flags.public { "0.0.0.0" } else { "127.0.0.1" };
    let bind_addr = flags
        .bind
        .unwrap_or_else(|| format!("{default_host}:{}", flags.port));
    let Some((listener, local_addr)) = bind_addr_socket(&bind_addr).await else {
        return ExitCode::from(1);
    };

    let pub_cfg = PublicConfig {
        public_mode: flags.public,
        rpm_limit: flags.rpm,
        max_body_bytes: flags.max_body_mb.saturating_mul(1024 * 1024),
        admin_token: flags.admin_token,
    };
    let mode_tag = if flags.public { "public" } else { "local" };
    eprintln!("[aphelion v3.2] listening on http://{local_addr} (mode={mode_tag}) -> {primary}");
    if !flags.public {
        eprintln!("[aphelion v3.2] local dashboard: http://{local_addr}/ui");
    }

    let state = Arc::new(ProxyState::with_config(
        targets,
        pub_cfg,
        audit.map(Mutex::new),
    ));
    let server_task = tokio::spawn(run_proxy_server(listener, Arc::clone(&state)));
    let _ = tokio::signal::ctrl_c().await;
    server_task.abort();
    print_summary(&state.summary_snapshot());
    ExitCode::SUCCESS
}

fn parse_flags(args: &[String], default_port: u16) -> CliFlags {
    let mut f = CliFlags {
        audit: false,
        public: false,
        admin_token: None,
        port: default_port,
        bind: None,
        rpm: 120,
        max_body_mb: 10,
        upstream: None,
        cmd_start: 0,
    };
    while f.cmd_start < args.len() {
        match args[f.cmd_start].as_str() {
            "--audit" => {
                f.audit = true;
                f.cmd_start += 1;
            }
            "--public" => {
                f.public = true;
                f.cmd_start += 1;
            }
            "--admin-token" if f.cmd_start + 1 < args.len() => {
                f.admin_token = Some(args[f.cmd_start + 1].clone());
                f.cmd_start += 2;
            }
            "--port" if f.cmd_start + 1 < args.len() => {
                f.port = args[f.cmd_start + 1].parse().unwrap_or(default_port);
                f.cmd_start += 2;
            }
            "--bind" if f.cmd_start + 1 < args.len() => {
                f.bind = Some(args[f.cmd_start + 1].clone());
                f.cmd_start += 2;
            }
            "--rpm" if f.cmd_start + 1 < args.len() => {
                f.rpm = args[f.cmd_start + 1].parse().unwrap_or(120);
                f.cmd_start += 2;
            }
            "--max-body-mb" if f.cmd_start + 1 < args.len() => {
                f.max_body_mb = args[f.cmd_start + 1].parse().unwrap_or(10);
                f.cmd_start += 2;
            }
            "--upstream" if f.cmd_start + 1 < args.len() => {
                f.upstream = Some(args[f.cmd_start + 1].clone());
                f.cmd_start += 2;
            }
            _ => break,
        }
    }
    f
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
    UpstreamTargets {
        openai,
        anthropic,
        gemini,
        ollama,
    }
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
