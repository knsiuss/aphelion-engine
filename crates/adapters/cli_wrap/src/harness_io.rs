use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy)]
pub enum ConfigFormat {
    JsonPath(&'static [&'static str]),
    TomlKey(&'static str),
    YamlKey(&'static str),
    ReadOnlyDir,
}

pub fn backup_path(target: &Path) -> PathBuf {
    let mut p = target.as_os_str().to_os_string();
    p.push(".aphelion.bak");
    PathBuf::from(p)
}

pub fn read_configured_url(path: &Path, fmt: ConfigFormat) -> Option<String> {
    if matches!(fmt, ConfigFormat::ReadOnlyDir) {
        return None;
    }
    let raw = std::fs::read_to_string(path).ok()?;
    match fmt {
        ConfigFormat::JsonPath(keys) => {
            let mut cur = serde_json::from_str::<Value>(&raw).ok()?;
            for k in keys {
                cur = cur.get(*k)?.clone();
            }
            cur.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
        }
        ConfigFormat::TomlKey(key) => read_kv_line(&raw, key, '='),
        ConfigFormat::YamlKey(key) => read_kv_line(&raw, key, ':'),
        ConfigFormat::ReadOnlyDir => None,
    }
}

fn read_kv_line(raw: &str, key: &str, sep: char) -> Option<String> {
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((lhs, rhs)) = trimmed.split_once(sep) {
            if lhs.trim() == key {
                let val = rhs.split('#').next().unwrap_or(rhs).trim();
                let unquoted = val.trim_matches('"').trim_matches('\'').trim();
                if !unquoted.is_empty() {
                    return Some(unquoted.to_owned());
                }
            }
        }
    }
    None
}

pub fn activate_config(path: &Path, fmt: ConfigFormat, url: &str) -> Result<(), String> {
    if matches!(fmt, ConfigFormat::ReadOnlyDir) {
        return Err("manual IDE settings required; copy base URL from Quick Connect".to_owned());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }
    let bak = backup_path(path);
    let existing_raw = if path.exists() {
        let raw = std::fs::read_to_string(path).map_err(|e| format!("read failed: {e}"))?;
        if !bak.exists() {
            std::fs::write(&bak, &raw).map_err(|e| format!("backup failed: {e}"))?;
        }
        Some(raw)
    } else {
        None
    };

    let updated = match fmt {
        ConfigFormat::JsonPath(keys) => {
            let mut root = match existing_raw.as_deref() {
                Some(raw) => serde_json::from_str::<Value>(raw)
                    .map_err(|e| format!("invalid json: {e}"))?,
                None => json!({}),
            };
            set_json_path(&mut root, keys, url)?;
            serde_json::to_string_pretty(&root).map_err(|e| format!("serialize failed: {e}"))?
        }
        ConfigFormat::TomlKey(key) => {
            let new_line = format!("{key} = \"{url}\"");
            upsert_kv_line(existing_raw.as_deref(), key, '=', &new_line, true)
        }
        ConfigFormat::YamlKey(key) => {
            let new_line = format!("{key}: {url}");
            upsert_kv_line(existing_raw.as_deref(), key, ':', &new_line, false)
        }
        ConfigFormat::ReadOnlyDir => unreachable!(),
    };
    std::fs::write(path, updated).map_err(|e| format!("write failed: {e}"))
}

pub fn deactivate_config(path: &Path, fmt: ConfigFormat) -> Result<(), String> {
    if matches!(fmt, ConfigFormat::ReadOnlyDir) {
        return Err("manual IDE settings required; revert in IDE preferences".to_owned());
    }
    let bak = backup_path(path);
    if bak.exists() {
        let orig = std::fs::read_to_string(&bak).map_err(|e| format!("read backup failed: {e}"))?;
        std::fs::write(path, orig).map_err(|e| format!("restore failed: {e}"))?;
        let _ = std::fs::remove_file(&bak);
        return Ok(());
    }
    if !path.exists() {
        return Ok(());
    }
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read failed: {e}"))?;
    let cleaned = match fmt {
        ConfigFormat::JsonPath(keys) => {
            if let Ok(mut root) = serde_json::from_str::<Value>(&raw) {
                remove_json_path(&mut root, keys);
                serde_json::to_string_pretty(&root).ok()
            } else {
                None
            }
        }
        ConfigFormat::TomlKey(key) => Some(remove_kv_line(&raw, key, '=')),
        ConfigFormat::YamlKey(key) => Some(remove_kv_line(&raw, key, ':')),
        ConfigFormat::ReadOnlyDir => None,
    };
    if let Some(text) = cleaned {
        let _ = std::fs::write(path, text);
    }
    Ok(())
}

fn set_json_path(root: &mut Value, keys: &[&str], url: &str) -> Result<(), String> {
    let mut cur = root;
    for (idx, k) in keys.iter().enumerate() {
        let obj = cur
            .as_object_mut()
            .ok_or_else(|| "config node must be a JSON object".to_owned())?;
        if idx + 1 == keys.len() {
            obj.insert((*k).to_owned(), Value::String(url.to_owned()));
            return Ok(());
        }
        cur = obj.entry((*k).to_owned()).or_insert_with(|| json!({}));
    }
    Ok(())
}

fn remove_json_path(root: &mut Value, keys: &[&str]) {
    let mut cur = root;
    for (idx, k) in keys.iter().enumerate() {
        let Some(obj) = cur.as_object_mut() else {
            return;
        };
        if idx + 1 == keys.len() {
            obj.remove(*k);
            return;
        }
        let Some(next) = obj.get_mut(*k) else {
            return;
        };
        cur = next;
    }
}

fn upsert_kv_line(raw: Option<&str>, key: &str, sep: char, new_line: &str, prepend: bool) -> String {
    let Some(content) = raw else {
        return format!("{new_line}\n");
    };
    let mut replaced = false;
    let mut lines: Vec<String> = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        let matches = !trimmed.is_empty()
            && !trimmed.starts_with('#')
            && trimmed.split_once(sep).is_some_and(|(lhs, _)| lhs.trim() == key);
        if matches && !replaced {
            lines.push(new_line.to_owned());
            replaced = true;
        } else if !matches {
            lines.push(line.to_owned());
        }
    }
    if !replaced {
        if prepend {
            lines.insert(0, new_line.to_owned());
        } else {
            lines.push(new_line.to_owned());
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

fn remove_kv_line(raw: &str, key: &str, sep: char) -> String {
    let lines: Vec<&str> = raw.lines().filter(|line| {
        let t = line.trim();
        t.is_empty() || t.starts_with('#') || !t.split_once(sep).is_some_and(|(lhs, _)| lhs.trim() == key)
    }).collect();
    if lines.is_empty() { String::new() } else { format!("{}\n", lines.join("\n")) }
}

pub fn strip_harness_prefix(path_and_query: &str) -> Option<(String, String)> {
    let after = path_and_query.strip_prefix("/h/")?;
    let (id, rest) = match after.find(['/', '?']) {
        Some(idx) => (&after[..idx], &after[idx..]),
        None => (after, "/"),
    };
    if id.is_empty() || id.contains('@') { return None; }
    let norm_rest = if rest.starts_with('/') { rest.to_owned() } else { format!("/{rest}") };
    Some((id.to_owned(), norm_rest))
}

pub fn default_harness_routes_path(home: &Path) -> PathBuf {
    home.join(".aphelion").join("harness_routes.json")
}

pub fn load_harness_routes_from(path: &Path) -> std::collections::HashMap<String, String> {
    std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save_harness_route_to(path: &Path, id: &str, url: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?; }
    let mut map = load_harness_routes_from(path);
    map.insert(id.to_owned(), url.trim().trim_end_matches('/').to_owned());
    let text = serde_json::to_string_pretty(&map).map_err(|e| format!("serialize failed: {e}"))?;
    std::fs::write(path, text).map_err(|e| format!("write failed: {e}"))
}
