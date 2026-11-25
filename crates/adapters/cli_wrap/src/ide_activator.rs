use crate::spawn::user_home_dir;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
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

fn claude_settings_path(home: &Path) -> PathBuf {
    home.join(".claude").join("settings.json")
}

fn backup_path(target: &Path) -> PathBuf {
    let mut p = target.as_os_str().to_os_string();
    p.push(".aphelion.bak");
    PathBuf::from(p)
}

fn is_local_aphelion(url: &str, proxy_base: &str) -> bool {
    let norm = url.trim().trim_end_matches('/');
    let p_norm = proxy_base.trim().trim_end_matches('/');
    let alt = p_norm.replace("127.0.0.1", "localhost");
    norm.eq_ignore_ascii_case(p_norm) || norm.eq_ignore_ascii_case(&alt)
}

/// Inspects Claude Code (`~/.claude/settings.json`) integration status without mutating anything.
pub fn inspect_integrations(proxy_base: &str) -> Vec<IntegrationStatus> {
    inspect_integrations_in(&user_home_dir(), proxy_base)
}

pub fn inspect_integrations_in(home: &Path, proxy_base: &str) -> Vec<IntegrationStatus> {
    let claude_path = claude_settings_path(home);
    let claude_detected = home.join(".claude").exists() || claude_path.exists();
    let current_url = std::fs::read_to_string(&claude_path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.pointer("/env/ANTHROPIC_BASE_URL").and_then(Value::as_str).map(str::to_owned));
    let active = current_url.as_deref().is_some_and(|u| is_local_aphelion(u, proxy_base));
    let can_restore = backup_path(&claude_path).exists();

    vec![IntegrationStatus {
        id: "claude_code".to_owned(),
        name: "Claude Code (CLI + VS Code / GUI)".to_owned(),
        detected: claude_detected,
        active,
        config_path: claude_path.display().to_string(),
        current_url,
        can_restore,
    }]
}

/// Activates Aphelion for a supported IDE (backs up existing config first; preserves all plugins/MCP).
pub fn activate_integration(id: &str, proxy_base: &str) -> Result<IntegrationStatus, String> {
    activate_integration_in(&user_home_dir(), id, proxy_base)
}

pub fn activate_integration_in(home: &Path, id: &str, proxy_base: &str) -> Result<IntegrationStatus, String> {
    if id != "claude_code" {
        return Err(format!("unsupported integration `{id}`"));
    }
    let path = claude_settings_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }
    let bak = backup_path(&path);
    let mut root = if path.exists() {
        let raw = std::fs::read_to_string(&path).map_err(|e| format!("read failed: {e}"))?;
        if !bak.exists() {
            std::fs::write(&bak, &raw).map_err(|e| format!("backup failed: {e}"))?;
        }
        serde_json::from_str::<Value>(&raw).map_err(|e| format!("invalid json: {e}"))?
    } else {
        json!({})
    };
    let obj = root.as_object_mut().ok_or_else(|| "settings.json root must be an object".to_owned())?;
    let env_val = obj.entry("env").or_insert_with(|| json!({}));
    let env_obj = env_val.as_object_mut().ok_or_else(|| "settings.json `env` must be an object".to_owned())?;
    env_obj.insert("ANTHROPIC_BASE_URL".to_owned(), Value::String(proxy_base.trim_end_matches('/').to_owned()));

    let pretty = serde_json::to_string_pretty(&root).map_err(|e| format!("serialize failed: {e}"))?;
    std::fs::write(&path, pretty).map_err(|e| format!("write failed: {e}"))?;
    inspect_integrations_in(home, proxy_base)
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| "status lookup failed".to_owned())
}

/// Deactivates Aphelion by restoring the `.aphelion.bak` backup (or removing `ANTHROPIC_BASE_URL`).
pub fn deactivate_integration(id: &str, proxy_base: &str) -> Result<IntegrationStatus, String> {
    deactivate_integration_in(&user_home_dir(), id, proxy_base)
}

pub fn deactivate_integration_in(home: &Path, id: &str, proxy_base: &str) -> Result<IntegrationStatus, String> {
    if id != "claude_code" {
        return Err(format!("unsupported integration `{id}`"));
    }
    let path = claude_settings_path(home);
    let bak = backup_path(&path);
    if bak.exists() {
        let orig = std::fs::read_to_string(&bak).map_err(|e| format!("read backup failed: {e}"))?;
        std::fs::write(&path, orig).map_err(|e| format!("restore failed: {e}"))?;
        let _ = std::fs::remove_file(&bak);
    } else if path.exists() {
        let raw = std::fs::read_to_string(&path).map_err(|e| format!("read failed: {e}"))?;
        if let Ok(mut root) = serde_json::from_str::<Value>(&raw) {
            if let Some(env_obj) = root.get_mut("env").and_then(Value::as_object_mut) {
                env_obj.remove("ANTHROPIC_BASE_URL");
            }
            if let Ok(pretty) = serde_json::to_string_pretty(&root) {
                let _ = std::fs::write(&path, pretty);
            }
        }
    }
    inspect_integrations_in(home, proxy_base)
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| "status lookup failed".to_owned())
}
