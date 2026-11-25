use crate::account_vault::{default_aphelion_dir, AccountVault};
use crate::quota_tracker::QuotaTracker;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::RwLock;

/// Dispatch strategy for a named combo (`failover` default, `flash_first`, or `round_robin`).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ComboStrategy {
    #[default]
    Failover,
    FlashFirst,
    RoundRobin,
}

/// Single tier entry inside a named combo (`combo:<name>`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComboTarget {
    pub model: String,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub upstream: Option<String>,
    #[serde(default)]
    pub keys: Vec<String>,
}

/// JSON configuration for named combos (`APHELION_COMBOS`, `~/.aphelion/combos.json`, or `/api/combos`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ComboConfig {
    #[serde(default)]
    pub combos: HashMap<String, Vec<ComboTarget>>,
    #[serde(default)]
    pub strategies: HashMap<String, ComboStrategy>,
}

/// Concrete upstream dispatch slot after combo expansion, vault resolution & key rotation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSlot {
    pub model: String,
    pub account_id: Option<String>,
    pub upstream: Option<String>,
    pub dialect: Option<String>,
    pub api_key: Option<String>,
    pub slot_key: String,
}

pub fn default_combos_path() -> PathBuf {
    if let Ok(p) = std::env::var("APHELION_COMBOS_FILE") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    let home_path = default_aphelion_dir().join("combos.json");
    if home_path.exists() {
        return home_path;
    }
    let local = PathBuf::from("aphelion_combos.json");
    if local.exists() {
        return local;
    }
    home_path
}

/// Thread-safe Combo & Multi-Account/Key Pool (zero hardcoded models or secrets).
pub struct ComboPool {
    config: RwLock<ComboConfig>,
    rr_counter: AtomicUsize,
    escalated_sessions: RwLock<HashSet<String>>,
}

impl Default for ComboPool {
    fn default() -> Self {
        Self::from_env()
    }
}

impl ComboPool {
    pub fn from_env() -> Self {
        Self {
            config: RwLock::new(load_combo_config_from_env()),
            rr_counter: AtomicUsize::new(0),
            escalated_sessions: RwLock::new(HashSet::new()),
        }
    }

    pub fn snapshot(&self) -> ComboConfig {
        self.config.read().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn replace(&self, next: ComboConfig) {
        if let Ok(mut g) = self.config.write() {
            *g = next;
        }
    }

    pub fn resolve_slots(&self, requested_model: &str, fallbacks: &[String]) -> Vec<ResolvedSlot> {
        self.resolve_slots_for_session(requested_model, fallbacks, None, false)
    }

    pub fn resolve_slots_for_session(
        &self,
        requested_model: &str,
        fallbacks: &[String],
        session_id: Option<&str>,
        escalate: bool,
    ) -> Vec<ResolvedSlot> {
        self.resolve_slots_with_vault(requested_model, fallbacks, session_id, escalate, None, None)
    }

    pub fn resolve_slots_with_vault(
        &self,
        requested_model: &str,
        fallbacks: &[String],
        session_id: Option<&str>,
        escalate: bool,
        vault: Option<&AccountVault>,
        quotas: Option<&QuotaTracker>,
    ) -> Vec<ResolvedSlot> {
        let req = requested_model.trim();
        if let Some(combo_name) = req.strip_prefix("combo:") {
            let (mut targets, strategy) = self
                .config
                .read()
                .ok()
                .map(|g| {
                    (
                        g.combos.get(combo_name).cloned().unwrap_or_default(),
                        g.strategies.get(combo_name).copied().unwrap_or_default(),
                    )
                })
                .unwrap_or_default();
            if !targets.is_empty() {
                let tick = self.rr_counter.fetch_add(1, Ordering::Relaxed);
                if strategy == ComboStrategy::FlashFirst
                    && targets.len() >= 2
                    && self.is_or_latch_escalated(session_id, escalate)
                {
                    targets.swap(0, 1);
                } else if strategy == ComboStrategy::RoundRobin && targets.len() >= 2 {
                    let shift = tick % targets.len();
                    targets.rotate_left(shift);
                }
                return expand_targets(&targets, tick, vault, quotas);
            }
        }

        let mut slots = Vec::with_capacity(1 + fallbacks.len());
        if !req.is_empty() {
            slots.push(plain_slot(req));
        }
        for fb in fallbacks {
            let trimmed = fb.trim();
            if !trimmed.is_empty() && !slots.iter().any(|s| s.model == trimmed) {
                slots.push(plain_slot(trimmed));
            }
        }
        slots
    }

    fn is_or_latch_escalated(&self, session_id: Option<&str>, escalate: bool) -> bool {
        let Some(sid) = session_id.filter(|s| !s.is_empty()) else {
            return escalate;
        };
        if escalate {
            if let Ok(mut g) = self.escalated_sessions.write() {
                g.insert(sid.to_owned());
            }
            return true;
        }
        self.escalated_sessions.read().map(|g| g.contains(sid)).unwrap_or(false)
    }
}

fn plain_slot(model: &str) -> ResolvedSlot {
    ResolvedSlot { model: model.to_owned(), account_id: None, upstream: None, dialect: None, api_key: None, slot_key: model.to_owned() }
}

fn expand_targets(targets: &[ComboTarget], tick: usize, vault: Option<&AccountVault>, quotas: Option<&QuotaTracker>) -> Vec<ResolvedSlot> {
    let (mut healthy, mut fallback_all) = (Vec::new(), Vec::new());
    for (tier_idx, target) in targets.iter().enumerate() {
        let model = target.model.trim();
        if model.is_empty() { continue; }
        if let Some(acct) = target.account_id.as_deref().and_then(|id| vault.and_then(|v| v.get(id))) {
            if !acct.enabled { continue; }
            let avail = quotas.is_none_or(|q| q.is_available(&acct.id, acct.daily_budget_usd));
            let slot = ResolvedSlot {
                model: model.to_owned(), account_id: Some(acct.id.clone()), upstream: Some(acct.base_url.clone()),
                dialect: Some(acct.dialect.clone()), api_key: (!acct.secret.trim().is_empty()).then(|| acct.secret.clone()),
                slot_key: format!("{model}#acct:{}", acct.id),
            };
            if avail { healthy.push(slot.clone()); }
            fallback_all.push(slot);
            continue;
        }
        if target.keys.is_empty() {
            let slot = ResolvedSlot { model: model.to_owned(), account_id: None, upstream: target.upstream.clone(), dialect: None, api_key: None, slot_key: format!("{model}#t{tier_idx}") };
            healthy.push(slot.clone());
            fallback_all.push(slot);
        } else {
            let n = target.keys.len();
            for offset in 0..n {
                let key_idx = (tick + offset) % n;
                let key = target.keys[key_idx].trim();
                if key.is_empty() { continue; }
                let slot = ResolvedSlot { model: model.to_owned(), account_id: None, upstream: target.upstream.clone(), dialect: None, api_key: Some(key.to_owned()), slot_key: format!("{model}#t{tier_idx}#k{key_idx}") };
                healthy.push(slot.clone());
                fallback_all.push(slot);
            }
        }
    }
    if healthy.is_empty() { fallback_all } else { healthy }
}

fn load_combo_config_from_env() -> ComboConfig {
    if let Ok(raw) = std::env::var("APHELION_COMBOS") {
        if let Ok(cfg) = serde_json::from_str::<ComboConfig>(&raw) {
            return cfg;
        }
    }
    if let Ok(raw) = std::fs::read_to_string(default_combos_path()) {
        if let Ok(cfg) = serde_json::from_str::<ComboConfig>(&raw) {
            return cfg;
        }
    }
    ComboConfig::default()
}
