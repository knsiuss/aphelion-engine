use crate::spawn::user_home_dir;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthType {
    #[default]
    ApiKey,
    OAuthBearer,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AccountTier {
    Free,
    Subscription,
    Cheap,
    #[default]
    Paid,
}

/// Persistent provider account stored in `~/.aphelion/accounts.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderAccount {
    pub id: String,
    #[serde(default)]
    pub label: String,
    pub provider: String,
    pub base_url: String,
    /// Upstream wire dialect: `"anthropic"` or `"openai"` (default `"openai"`).
    #[serde(default = "default_dialect")]
    pub dialect: String,
    #[serde(default)]
    pub auth_type: AuthType,
    #[serde(default)]
    pub secret: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_at: Option<u64>,
    #[serde(default)]
    pub tier: AccountTier,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub daily_budget_usd: Option<f64>,
}

fn default_dialect() -> String {
    "openai".to_owned()
}

fn default_true() -> bool {
    true
}

/// Safe API/UI representation with secret strictly masked.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactedAccount {
    pub id: String,
    pub label: String,
    pub provider: String,
    pub base_url: String,
    pub dialect: String,
    pub auth_type: AuthType,
    pub secret_masked: String,
    pub tier: AccountTier,
    pub enabled: bool,
    pub daily_budget_usd: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct VaultFile {
    #[serde(default)]
    accounts: Vec<ProviderAccount>,
}

pub fn mask_secret(secret: &str) -> String {
    let trimmed = secret.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= 8 {
        return "****".to_owned();
    }
    let head: String = chars[..3].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}...{tail}")
}

impl ProviderAccount {
    pub fn redacted(&self) -> RedactedAccount {
        RedactedAccount {
            id: self.id.clone(),
            label: if self.label.is_empty() { self.id.clone() } else { self.label.clone() },
            provider: self.provider.clone(),
            base_url: self.base_url.clone(),
            dialect: self.dialect.clone(),
            auth_type: self.auth_type,
            secret_masked: mask_secret(&self.secret),
            tier: self.tier,
            enabled: self.enabled,
            daily_budget_usd: self.daily_budget_usd,
        }
    }
}

pub fn default_aphelion_dir() -> PathBuf {
    user_home_dir().join(".aphelion")
}

pub fn default_accounts_path() -> PathBuf {
    if let Ok(p) = std::env::var("APHELION_ACCOUNTS_FILE") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    default_aphelion_dir().join("accounts.json")
}

/// Thread-safe multi-account credential vault.
pub struct AccountVault {
    path: PathBuf,
    accounts: RwLock<Vec<ProviderAccount>>,
}

impl Default for AccountVault {
    fn default() -> Self {
        Self::load_from_path(default_accounts_path())
    }
}

impl AccountVault {
    pub fn load_from_path(path: PathBuf) -> Self {
        let loaded = load_file(&path).unwrap_or_default();
        Self {
            path,
            accounts: RwLock::new(loaded),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn list_redacted(&self) -> Vec<RedactedAccount> {
        self.accounts
            .read()
            .map(|g| g.iter().map(ProviderAccount::redacted).collect())
            .unwrap_or_default()
    }

    pub fn get(&self, id: &str) -> Option<ProviderAccount> {
        self.accounts
            .read()
            .ok()?
            .iter()
            .find(|a| a.id == id)
            .cloned()
    }

    pub fn upsert(&self, mut acct: ProviderAccount) -> Result<RedactedAccount, String> {
        acct.id = acct.id.trim().to_owned();
        if acct.id.is_empty() || acct.base_url.trim().is_empty() {
            return Err("account `id` and `base_url` are required".to_owned());
        }
        let redacted = acct.redacted();
        let snapshot = {
            let mut guard = self.accounts.write().map_err(|_| "vault lock poisoned".to_owned())?;
            if let Some(existing) = guard.iter_mut().find(|a| a.id == acct.id) {
                if acct.secret.trim().is_empty() {
                    acct.secret = existing.secret.clone();
                }
                *existing = acct;
            } else {
                guard.push(acct);
            }
            guard.clone()
        };
        save_file(&self.path, &snapshot)?;
        Ok(redacted)
    }

    pub fn remove(&self, id: &str) -> Result<bool, String> {
        let (removed, snapshot) = {
            let mut guard = self.accounts.write().map_err(|_| "vault lock poisoned".to_owned())?;
            let before = guard.len();
            guard.retain(|a| a.id != id);
            (guard.len() < before, guard.clone())
        };
        if removed {
            save_file(&self.path, &snapshot)?;
        }
        Ok(removed)
    }
}

fn load_file(path: &Path) -> Option<Vec<ProviderAccount>> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<VaultFile>(&raw)
        .map(|f| f.accounts)
        .or_else(|_| serde_json::from_str::<Vec<ProviderAccount>>(&raw))
        .ok()
}

fn save_file(path: &Path, accounts: &[ProviderAccount]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create dir failed: {e}"))?;
        }
    }
    let payload = VaultFile { accounts: accounts.to_vec() };
    let text = serde_json::to_string_pretty(&payload).map_err(|e| format!("serialize failed: {e}"))?;
    std::fs::write(path, text).map_err(|e| format!("write accounts failed: {e}"))
}
