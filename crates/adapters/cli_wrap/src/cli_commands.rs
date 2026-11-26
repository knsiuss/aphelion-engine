use crate::config::{GatewayConfig, DEFAULT_GATEWAY_PORT, DEFAULT_LOCAL_HOST};
use crate::ide_activator::inspect_integrations;
use crate::spawn::{detect_claude_settings_base_url, user_home_dir};
use std::process::{Command, ExitCode};

/// `aphelion doctor`: detects local agent harnesses & IDEs using unified registry
/// without modifying user secrets.
pub fn run_doctor() {
    let home = user_home_dir();
    let port = GatewayConfig::env_port(DEFAULT_GATEWAY_PORT);
    let anthropic_base = format!("http://{DEFAULT_LOCAL_HOST}:{port}");

    println!("Aphelion v3.8 Doctor — Universal Agent Harness Onboarding");
    println!("Start gateway: aphelion serve --port {port} --audit");
    println!("Open dashboard: aphelion ui --port {port}\n");

    for item in inspect_integrations(&anthropic_base) {
        let status = if item.active {
            "ACTIVE"
        } else if item.detected {
            "DETECTED"
        } else {
            "not found"
        };
        println!("  [{status:^9}] {:<32} -> {}", item.name, item.config_path);
    }

    if let Some(claude_url) = detect_claude_settings_base_url(&home) {
        let normalized = claude_url.trim_end_matches('/');
        let local_ip = format!("http://{DEFAULT_LOCAL_HOST}:{port}");
        let local_host = format!("http://localhost:{port}");
        if normalized != local_ip && normalized != local_host {
            println!(
                "\n  [WARNING] ~/.claude/settings.json sets env.ANTHROPIC_BASE_URL = \"{claude_url}\""
            );
            println!(
                "            This overrides env-injected URLs; `aphelion claude` auto-injects --settings, or point it to {anthropic_base}."
            );
        }
    }
}

/// `aphelion ui [--port <u16>]`: opens the embedded local telemetry dashboard in the default browser.
pub fn run_ui_open(args: &[String]) -> ExitCode {
    let mut port = GatewayConfig::env_port(DEFAULT_GATEWAY_PORT);
    let mut host = DEFAULT_LOCAL_HOST.to_owned();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--port" if i + 1 < args.len() => {
                port = args[i + 1].parse().unwrap_or(port);
                i += 2;
            }
            "--host" if i + 1 < args.len() => {
                host = args[i + 1].clone();
                i += 2;
            }
            _ => i += 1,
        }
    }
    let url = format!("http://{host}:{port}/ui");
    println!("[aphelion] dashboard URL: {url}");
    let launched = if cfg!(target_os = "windows") {
        Command::new("cmd").args(["/C", "start", "", &url]).spawn()
    } else if cfg!(target_os = "macos") {
        Command::new("open").arg(&url).spawn()
    } else {
        Command::new("xdg-open").arg(&url).spawn()
    };
    if let Err(err) = launched {
        eprintln!("[aphelion] open {url} in your browser manually ({err})");
    }
    ExitCode::SUCCESS
}
