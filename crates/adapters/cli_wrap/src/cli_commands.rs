use crate::config::{GatewayConfig, DEFAULT_GATEWAY_PORT, DEFAULT_LOCAL_HOST};
use crate::spawn::{detect_claude_settings_base_url, user_home_dir};
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn user_config_dir(home: &Path) -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .or_else(|| env::var_os("APPDATA"))
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"))
}

/// `aphelion doctor`: detects local IDEs using cross-OS paths and prints dynamic `baseURL` hints
/// without reading or modifying any user secrets.
pub fn run_doctor() {
    let home = user_home_dir();
    let cfg_dir = user_config_dir(&home);
    let port = GatewayConfig::env_port(DEFAULT_GATEWAY_PORT);
    let base = format!("http://{DEFAULT_LOCAL_HOST}:{port}/v1");
    let anthropic_base = format!("http://{DEFAULT_LOCAL_HOST}:{port}");

    println!("Aphelion v3.3 Doctor — Zero-Config IDE Onboarding");
    println!("Start gateway: aphelion serve --port {port} --audit");
    println!("Open dashboard: aphelion ui --port {port}\n");

    let opencode_paths = [cfg_dir.join("opencode"), home.join(".config").join("opencode")];
    let checks: [(&str, bool, String); 5] = [
        (
            "ClaudeCode",
            home.join(".claude").exists(),
            format!(r#"Set ANTHROPIC_BASE_URL: "{anthropic_base}" or run `aphelion claude`"#),
        ),
        (
            "OpenCode",
            opencode_paths.iter().any(|p| p.exists()),
            format!(r#"Set provider baseURL: "{base}""#),
        ),
        (
            "Cursor",
            home.join(".cursor").exists(),
            format!("Settings -> Models -> Override OpenAI Base URL: {base}"),
        ),
        (
            "Cline",
            home.join(".vscode").join("extensions").exists(),
            format!("Cline Settings -> OpenAI Compatible Base URL: {base}"),
        ),
        (
            "Continue",
            home.join(".continue").exists(),
            format!("config.yaml -> apiBase: {base}"),
        ),
    ];
    for (name, found, hint) in checks {
        let status = if found { "DETECTED" } else { "not found" };
        println!("  [{status:^9}] {name:<10} -> {hint}");
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
