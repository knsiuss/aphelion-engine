use serde_json::{json, Value};
use std::env;
use std::path::{Path, PathBuf};
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedExec {
    Direct(PathBuf),
    CmdBatch(PathBuf),
    PowerShell(PathBuf),
}

/// Resolves `program` against a list of `PATH` directories using Windows `PATHEXT` priority
/// (`.exe`, `.cmd`, `.bat`, `.ps1`). Pure helper for deterministic cross-platform testing.
pub fn resolve_windows_executable(
    program: &str,
    path_dirs: &[PathBuf],
    pathext: &[&str],
) -> Option<ResolvedExec> {
    let direct = Path::new(program);
    if direct.is_file() {
        return Some(classify_exec(direct.to_path_buf()));
    }
    let has_ext = direct.extension().is_some();
    for dir in path_dirs {
        if has_ext {
            let cand = dir.join(program);
            if cand.is_file() {
                return Some(classify_exec(cand));
            }
            continue;
        }
        for ext in pathext {
            let norm_ext = if ext.starts_with('.') {
                ext.to_ascii_lowercase()
            } else {
                format!(".{}", ext.to_ascii_lowercase())
            };
            let cand = dir.join(format!("{program}{norm_ext}"));
            if cand.is_file() {
                return Some(classify_exec(cand));
            }
        }
    }
    None
}

fn classify_exec(path: PathBuf) -> ResolvedExec {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "cmd" | "bat" => ResolvedExec::CmdBatch(path),
        "ps1" => ResolvedExec::PowerShell(path),
        _ => ResolvedExec::Direct(path),
    }
}

/// Cross-OS user home directory resolution (`USERPROFILE` on Windows, `HOME` on POSIX).
pub fn user_home_dir() -> PathBuf {
    env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Reads `<home>/.claude/settings.json` and returns `env.ANTHROPIC_BASE_URL` if configured.
pub fn detect_claude_settings_base_url(home: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(home.join(".claude").join("settings.json")).ok()?;
    let val: Value = serde_json::from_str(&raw).ok()?;
    val.pointer("/env/ANTHROPIC_BASE_URL")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn is_claude_program(program: &str) -> bool {
    Path::new(program)
        .file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case("claude"))
}

fn prepare_claude_settings_override(
    program: &str,
    args: &[String],
    proxy_base: &str,
    home: &Path,
) -> Option<Vec<String>> {
    if !is_claude_program(program) || args.iter().any(|a| a == "--settings") {
        return None;
    }
    let existing = detect_claude_settings_base_url(home)?;
    if existing.trim_end_matches('/') == proxy_base.trim_end_matches('/') {
        return None;
    }
    let tmp_path = env::temp_dir().join(format!(
        "aphelion_claude_settings_{}.json",
        std::process::id()
    ));
    let payload = json!({ "env": { "ANTHROPIC_BASE_URL": proxy_base } });
    if std::fs::write(&tmp_path, payload.to_string()).is_ok() {
        eprintln!(
            "[aphelion] detected ANTHROPIC_BASE_URL={existing} in ~/.claude/settings.json; injecting --settings override -> {proxy_base}"
        );
        let mut updated = args.to_vec();
        updated.push("--settings".to_owned());
        updated.push(tmp_path.to_string_lossy().into_owned());
        return Some(updated);
    }
    None
}

/// Builds a child `tokio::process::Command` with native Windows `.cmd`/`.bat`/`.ps1`
/// resolution, Claude Code `settings.json` override guard, and universal base URL env injection.
pub fn build_child_command(program: &str, args: &[String], proxy_base: &str) -> Command {
    let home = user_home_dir();
    let effective_args = prepare_claude_settings_override(program, args, proxy_base, &home);
    let final_args = effective_args.as_deref().unwrap_or(args);
    let mut cmd = build_platform_command(program, final_args);
    inject_universal_env(&mut cmd, proxy_base);
    cmd
}

fn build_platform_command(program: &str, args: &[String]) -> Command {
    #[cfg(windows)]
    {
        let path_dirs: Vec<PathBuf> = env::var_os("PATH")
            .map(|p| env::split_paths(&p).collect())
            .unwrap_or_default();
        let pathext_raw = env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT;.PS1".to_owned());
        let mut exts: Vec<&str> = pathext_raw.split(';').filter(|s| !s.is_empty()).collect();
        for fallback in [".exe", ".cmd", ".bat", ".ps1"] {
            if !exts.iter().any(|e| e.eq_ignore_ascii_case(fallback)) {
                exts.push(fallback);
            }
        }
        if let Some(resolved) = resolve_windows_executable(program, &path_dirs, &exts) {
            return match resolved {
                ResolvedExec::Direct(p) => {
                    let mut c = Command::new(p);
                    c.args(args);
                    c
                }
                ResolvedExec::CmdBatch(p) => {
                    let mut c = Command::new("cmd.exe");
                    c.arg("/C").arg(p).args(args);
                    c
                }
                ResolvedExec::PowerShell(p) => {
                    let mut c = Command::new("powershell.exe");
                    c.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
                        .arg(p)
                        .args(args);
                    c
                }
            };
        }
    }
    let mut c = Command::new(program);
    c.args(args);
    c
}

fn inject_universal_env(cmd: &mut Command, proxy_base: &str) {
    let v1 = format!("{proxy_base}/v1");
    cmd.env("OPENAI_BASE_URL", &v1)
        .env("OPENAI_API_BASE", &v1)
        .env("OPENROUTER_BASE_URL", &v1)
        .env("DEEPSEEK_BASE_URL", &v1)
        .env("ANTHROPIC_BASE_URL", proxy_base)
        .env("GEMINI_API_BASE_URL", proxy_base)
        .env("GOOGLE_GEMINI_BASE_URL", proxy_base)
        .env("OLLAMA_HOST", proxy_base);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_pathext_resolves_exe_cmd_and_ps1_priority() {
        let dir = std::env::temp_dir().join("aphelion_pathext_test");
        let _ = std::fs::create_dir_all(&dir);
        let cmd_file = dir.join("opencode.cmd");
        let ps1_file = dir.join("claude.ps1");
        let exe_file = dir.join("aider.exe");
        std::fs::write(&cmd_file, "@echo off").unwrap();
        std::fs::write(&ps1_file, "Write-Host hi").unwrap();
        std::fs::write(&exe_file, "MZ").unwrap();

        let exts = [".exe", ".cmd", ".bat", ".ps1"];
        let dirs = std::slice::from_ref(&dir);
        assert_eq!(
            resolve_windows_executable("opencode", dirs, &exts),
            Some(ResolvedExec::CmdBatch(cmd_file))
        );
        assert_eq!(
            resolve_windows_executable("claude", dirs, &exts),
            Some(ResolvedExec::PowerShell(ps1_file))
        );
        assert_eq!(
            resolve_windows_executable("aider", dirs, &exts),
            Some(ResolvedExec::Direct(exe_file))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
