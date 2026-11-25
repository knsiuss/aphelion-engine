use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default)]
struct AccountQuotaState {
    requests_today: u64,
    tokens_today: u64,
    cost_today_usd: f64,
    cooldown_until: Option<Instant>,
    last_error: Option<u16>,
}

/// Serializable per-account health & quota snapshot for `/api/accounts` and `/ui`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountQuotaSnapshot {
    pub status: String,
    pub cooldown_remaining_secs: u64,
    pub requests_today: u64,
    pub tokens_today: u64,
    pub cost_today_usd: f64,
    pub last_error: Option<u16>,
}

/// Live in-memory quota, daily budget, and rate-limit cooldown tracker per provider account.
#[derive(Default)]
pub struct QuotaTracker {
    states: RwLock<HashMap<String, AccountQuotaState>>,
}

impl QuotaTracker {
    pub fn is_available(&self, account_id: &str, daily_budget_usd: Option<f64>) -> bool {
        let Ok(guard) = self.states.read() else {
            return true;
        };
        let Some(st) = guard.get(account_id) else {
            return true;
        };
        if let Some(until) = st.cooldown_until {
            if Instant::now() < until {
                return false;
            }
        }
        if let Some(limit) = daily_budget_usd {
            if limit > 0.0 && st.cost_today_usd >= limit {
                return false;
            }
        }
        true
    }

    pub fn record_usage(&self, account_id: &str, tokens: u64, cost_usd: f64) {
        if account_id.is_empty() {
            return;
        }
        if let Ok(mut guard) = self.states.write() {
            let st = guard.entry(account_id.to_owned()).or_default();
            st.requests_today += 1;
            st.tokens_today += tokens;
            st.cost_today_usd += cost_usd.max(0.0);
        }
    }

    pub fn mark_cooldown(&self, account_id: &str, status_code: u16, secs: u64) {
        if account_id.is_empty() {
            return;
        }
        if let Ok(mut guard) = self.states.write() {
            let st = guard.entry(account_id.to_owned()).or_default();
            st.last_error = Some(status_code);
            st.cooldown_until = Some(Instant::now() + Duration::from_secs(secs.max(1)));
        }
    }

    pub fn snapshot(&self, account_id: &str, daily_budget_usd: Option<f64>) -> AccountQuotaSnapshot {
        let st = self
            .states
            .read()
            .ok()
            .and_then(|g| g.get(account_id).cloned())
            .unwrap_or_default();
        let now = Instant::now();
        let cooldown_remaining_secs = st
            .cooldown_until
            .and_then(|u| u.checked_duration_since(now))
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let budget_hit = daily_budget_usd.is_some_and(|lim| lim > 0.0 && st.cost_today_usd >= lim);
        let status = if cooldown_remaining_secs > 0 {
            "cooldown"
        } else if budget_hit {
            "budget_exhausted"
        } else {
            "healthy"
        };
        AccountQuotaSnapshot {
            status: status.to_owned(),
            cooldown_remaining_secs,
            requests_today: st.requests_today,
            tokens_today: st.tokens_today,
            cost_today_usd: (st.cost_today_usd * 100_000.0).round() / 100_000.0,
            last_error: st.last_error,
        }
    }
}
