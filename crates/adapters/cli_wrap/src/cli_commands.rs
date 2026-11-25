use std::env;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

/// `aphelion doctor`: detects local IDEs and prints 1-line `baseURL` integration snippets
/// without reading or modifying any user secrets.
pub fn run_doctor() {
    let home = env::var("USERPROFILE")
        .or_else(|_| env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));

    println!("Aphelion v3.2 Doctor — Zero-Config IDE Onboarding");
    println!("Start gateway: aphelion serve --port 18787 --audit");
    println!("Open dashboard: aphelion ui --port 18787\n");

    let checks = [
        (
            "OpenCode",
            home.join(".config/opencode"),
            r#"Set provider baseURL: "http://127.0.0.1:18787/v1""#,
        ),
        (
            "Cursor",
            home.join(".cursor"),
            "Settings -> Models -> Override OpenAI Base URL: http://127.0.0.1:18787/v1",
        ),
        (
            "Cline",
            home.join(".vscode/extensions"),
            "Cline Settings -> OpenAI Compatible Base URL: http://127.0.0.1:18787/v1",
        ),
        (
            "Continue",
            home.join(".continue"),
            "config.yaml -> apiBase: http://127.0.0.1:18787/v1",
        ),
    ];
    for (name, path, hint) in checks {
        let status = if path.exists() { "DETECTED" } else { "not found" };
        println!("  [{status:^9}] {name:<10} -> {hint}");
    }
}

/// `aphelion ui [--port 18787]`: opens the embedded local telemetry dashboard in the default browser.
pub fn run_ui_open(args: &[String]) -> ExitCode {
    let mut port = 18787u16;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--port" && i + 1 < args.len() {
            port = args[i + 1].parse().unwrap_or(18787);
            i += 2;
        } else {
            i += 1;
        }
    }
    let url = format!("http://127.0.0.1:{port}/ui");
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
