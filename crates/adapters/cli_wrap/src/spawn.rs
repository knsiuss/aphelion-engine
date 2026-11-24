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

/// Builds a child `tokio::process::Command` with native Windows `.cmd`/`.bat`/`.ps1`
/// resolution and universal AI harness/SDK base URL environment injection.
pub fn build_child_command(program: &str, args: &[String], proxy_base: &str) -> Command {
    let mut cmd = build_platform_command(program, args);
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
