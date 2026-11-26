use crate::harness_io::{
    activate_config, backup_path, deactivate_config, read_configured_url, ConfigFormat,
};
use crate::spawn::user_home_dir;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrationStatus {
    pub id: String,
    pub name: String,
    pub detected: bool,
    pub active: bool,
    pub config_path: String,
    pub current_url: Option<String>,
    pub can_restore: bool,
}

#[derive(Clone, Copy)]
struct HarnessSpec {
    id: &'static str,
    name: &'static str,
    fmt: ConfigFormat,
    use_v1: bool,
}

const HARNESSES: &[HarnessSpec] = &[
    HarnessSpec {
        id: "claude_code",
        name: "Claude Code (CLI + VS Code / GUI)",
        fmt: ConfigFormat::JsonPath(&["env", "ANTHROPIC_BASE_URL"]),
        use_v1: false,
    },
    HarnessSpec {
        id: "mistral_vibe",
        name: "Mistral Vibe / CLI",
        fmt: ConfigFormat::TomlKey("api_base"),
        use_v1: true,
    },
    HarnessSpec {
        id: "codex_cli",
        name: "OpenAI Codex CLI",
        fmt: ConfigFormat::TomlKey("base_url"),
        use_v1: true,
    },
    HarnessSpec {
        id: "opencode",
        name: "OpenCode",
        fmt: ConfigFormat::JsonPath(&["provider", "openai", "options", "baseURL"]),
        use_v1: true,
    },
    HarnessSpec {
        id: "gemini_cli",
        name: "Gemini CLI",
        fmt: ConfigFormat::JsonPath(&["env", "GOOGLE_GEMINI_BASE_URL"]),
        use_v1: false,
    },
    HarnessSpec {
        id: "aider",
        name: "Aider",
        fmt: ConfigFormat::YamlKey("openai-api-base"),
        use_v1: true,
    },
    HarnessSpec {
        id: "cursor",
        name: "Cursor IDE",
        fmt: ConfigFormat::ReadOnlyDir,
        use_v1: true,
    },
    HarnessSpec {
        id: "windsurf",
        name: "Windsurf IDE",
        fmt: ConfigFormat::ReadOnlyDir,
        use_v1: true,
    },
    HarnessSpec {
        id: "cline_roo",
        name: "Cline / Roo Code (VS Code)",
        fmt: ConfigFormat::ReadOnlyDir,
        use_v1: true,
    },
    HarnessSpec {
        id: "continue_dev",
        name: "Continue",
        fmt: ConfigFormat::ReadOnlyDir,
        use_v1: true,
    },
];

fn resolve_paths(home: &Path, id: &str) -> (PathBuf, bool) {
    match id {
        "claude_code" => (
            home.join(".claude").join("settings.json"),
            home.join(".claude").exists() || home.join(".claude").join("settings.json").exists(),
        ),
        "mistral_vibe" => {
            let vibe = home.join(".vibe").join("config.toml");
            let mistral = home.join(".mistral").join("config.toml");
            let p = if !home.join(".vibe").exists() && home.join(".mistral").exists() {
                mistral
            } else {
                vibe
            };
            let det = home.join(".vibe").exists() || home.join(".mistral").exists();
            (p, det)
        }
        "codex_cli" => (
            home.join(".codex").join("config.toml"),
            home.join(".codex").exists(),
        ),
        "opencode" => {
            let xdg = home.join(".config").join("opencode").join("opencode.json");
            let appdata = std::env::var_os("APPDATA")
                .map(|a| PathBuf::from(a).join("opencode").join("opencode.json"));
            let det = home.join(".config").join("opencode").exists()
                || appdata.as_ref().is_some_and(|p| p.parent().is_some_and(|d| d.exists()));
            let path = if !home.join(".config").join("opencode").exists() {
                appdata.unwrap_or(xdg)
            } else {
                xdg
            };
            (path, det)
        }
        "gemini_cli" => (
            home.join(".gemini").join("settings.json"),
            home.join(".gemini").exists(),
        ),
        "aider" => {
            let p = home.join(".aider.conf.yml");
            (p.clone(), p.exists())
        }
        "cursor" => {
            let p = home.join(".cursor");
            (p.clone(), p.exists())
        }
        "windsurf" => {
            let p = home.join(".codeium").join("windsurf");
            (p.clone(), p.exists())
        }
        "cline_roo" => {
            let p = home.join(".vscode").join("extensions");
            (p.clone(), p.exists())
        }
        "continue_dev" => {
            let p = home.join(".continue").join("config.yaml");
            (p.clone(), home.join(".continue").exists() || p.exists())
        }
        _ => (home.to_path_buf(), false),
    }
}

fn effective_url(proxy_base: &str, use_v1: bool) -> String {
    let base = proxy_base.trim().trim_end_matches('/');
    if use_v1 { format!("{base}/v1") } else { base.to_owned() }
}

fn is_local_aphelion(url: &str, expected: &str) -> bool {
    let norm = url.trim().trim_end_matches('/');
    let e_norm = expected.trim().trim_end_matches('/');
    let alt = e_norm.replace("127.0.0.1", "localhost");
    norm.eq_ignore_ascii_case(e_norm) || norm.eq_ignore_ascii_case(&alt)
}

fn build_status(home: &Path, spec: &HarnessSpec, proxy_base: &str) -> IntegrationStatus {
    let (path, detected) = resolve_paths(home, spec.id);
    let expected = effective_url(proxy_base, spec.use_v1);
    let current_url = read_configured_url(&path, spec.fmt);
    let active = current_url.as_deref().is_some_and(|u| is_local_aphelion(u, &expected));
    let can_restore = !matches!(spec.fmt, ConfigFormat::ReadOnlyDir) && backup_path(&path).exists();
    IntegrationStatus {
        id: spec.id.to_owned(),
        name: spec.name.to_owned(),
        detected: detected || current_url.is_some(),
        active,
        config_path: path.display().to_string(),
        current_url,
        can_restore,
    }
}

pub fn inspect_integrations(proxy_base: &str) -> Vec<IntegrationStatus> {
    inspect_integrations_in(&user_home_dir(), proxy_base)
}

pub fn inspect_integrations_in(home: &Path, proxy_base: &str) -> Vec<IntegrationStatus> {
    let mut list: Vec<IntegrationStatus> = HARNESSES
        .iter()
        .map(|s| build_status(home, s, proxy_base))
        .collect();
    list.sort_by_key(|item| !item.detected);
    list
}

pub fn activate_integration(id: &str, proxy_base: &str) -> Result<IntegrationStatus, String> {
    activate_integration_in(&user_home_dir(), id, proxy_base)
}

pub fn activate_integration_in(home: &Path, id: &str, proxy_base: &str) -> Result<IntegrationStatus, String> {
    let spec = HARNESSES.iter().find(|s| s.id == id)
        .ok_or_else(|| format!("unsupported integration `{id}`"))?;
    let (path, _) = resolve_paths(home, id);
    let url = effective_url(proxy_base, spec.use_v1);
    activate_config(&path, spec.fmt, &url)?;
    Ok(build_status(home, spec, proxy_base))
}

pub fn deactivate_integration(id: &str, proxy_base: &str) -> Result<IntegrationStatus, String> {
    deactivate_integration_in(&user_home_dir(), id, proxy_base)
}

pub fn deactivate_integration_in(home: &Path, id: &str, proxy_base: &str) -> Result<IntegrationStatus, String> {
    let spec = HARNESSES.iter().find(|s| s.id == id)
        .ok_or_else(|| format!("unsupported integration `{id}`"))?;
    let (path, _) = resolve_paths(home, id);
    deactivate_config(&path, spec.fmt)?;
    Ok(build_status(home, spec, proxy_base))
}
