use crate::combo_pool::ResolvedSlot;
use crate::headers::{apply_proxy_headers, join_base_path};
use crate::quota_tracker::QuotaTracker;
use aphelion_core::defaults::DEFAULT_FAILOVER_COOLDOWN_SECS;
use aphelion_core::dialect_bridge::translate_anthropic_to_openai_request;
use hyper::HeaderMap;
use reqwest::{Client, Response};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Outcome of upstream dispatch including dialect-bridge status and chosen account/model.
pub struct DispatchResult {
    pub response: Response,
    pub model: String,
    pub account_id: Option<String>,
    pub did_failover: bool,
    pub bridged_oai_to_anthropic: bool,
}

/// In-memory per-slot cooldown tracker for automatic 402/429/500/502/503 failover.
#[derive(Debug, Default)]
pub struct CooldownTracker {
    entries: Mutex<HashMap<String, Instant>>,
}

impl CooldownTracker {
    pub fn is_cooling_down(&self, slot_key: &str) -> bool {
        let Ok(mut map) = self.entries.lock() else {
            return false;
        };
        let now = Instant::now();
        if let Some(&until) = map.get(slot_key) {
            if until > now {
                return true;
            }
            map.remove(slot_key);
        }
        false
    }

    pub fn mark_cooldown(&self, slot_key: &str, retry_after_secs: Option<u64>) {
        if slot_key.is_empty() {
            return;
        }
        let secs = retry_after_secs
            .unwrap_or(DEFAULT_FAILOVER_COOLDOWN_SECS)
            .clamp(5, 600);
        if let Ok(mut map) = self.entries.lock() {
            map.insert(slot_key.to_owned(), Instant::now() + Duration::from_secs(secs));
        }
    }
}

pub fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
}

pub fn rewrite_body_model(body: &[u8], new_model: &str) -> Option<Vec<u8>> {
    let mut val = serde_json::from_slice::<Value>(body).ok()?;
    let obj = val.as_object_mut()?;
    if !obj.contains_key("model") {
        return None;
    }
    obj.insert("model".to_owned(), Value::String(new_model.to_owned()));
    serde_json::to_vec(&val).ok()
}

fn build_slot_headers(base: &HeaderMap, api_key: Option<&str>, force_bearer: bool) -> HeaderMap {
    let mut h = base.clone();
    if force_bearer {
        h.remove("x-api-key");
        h.remove("anthropic-version");
        h.remove("anthropic-beta");
    }
    let Some(key) = api_key else {
        return h;
    };
    if !force_bearer && h.contains_key("x-api-key") {
        if let Ok(val) = hyper::header::HeaderValue::from_str(key) {
            h.insert("x-api-key", val);
        }
    } else if let Ok(val) = hyper::header::HeaderValue::from_str(&format!("Bearer {key}")) {
        h.insert("authorization", val);
    }
    h
}

/// Dispatches upstream request across resolved combo/account slots with per-slot cooldown,
/// native Anthropic -> OpenAI cross-dialect translation, and transparent cascading retries.
#[allow(clippy::too_many_arguments)]
pub async fn dispatch_with_failover(
    client: &Client,
    method: reqwest::Method,
    default_target_url: &str,
    path_and_query: &str,
    headers: &HeaderMap,
    initial_body: Vec<u8>,
    slots: &[ResolvedSlot],
    tracker: &CooldownTracker,
    quotas: &QuotaTracker,
) -> Option<DispatchResult> {
    if slots.is_empty() {
        let req = apply_proxy_headers(
            client.request(method, default_target_url),
            headers,
            initial_body.len(),
        )
        .body(initial_body);
        let res = req.send().await.ok()?;
        return Some(DispatchResult {
            response: res,
            model: String::new(),
            account_id: None,
            did_failover: false,
            bridged_oai_to_anthropic: false,
        });
    }

    let ordered: Vec<&ResolvedSlot> = {
        let healthy: Vec<&ResolvedSlot> = slots
            .iter()
            .filter(|s| !tracker.is_cooling_down(&s.slot_key))
            .collect();
        if healthy.is_empty() { slots.iter().collect() } else { healthy }
    };

    let is_anthropic_in = path_and_query.starts_with("/v1/messages");
    let mut did_failover = ordered[0].slot_key != slots[0].slot_key;
    for (hop, slot) in ordered.iter().take(4).enumerate() {
        let bridge = is_anthropic_in
            && slot
                .dialect
                .as_deref()
                .is_some_and(|d| d.eq_ignore_ascii_case("openai"));
        let mut body = rewrite_body_model(&initial_body, &slot.model)
            .unwrap_or_else(|| initial_body.clone());
        let effective_path = if bridge {
            if let Ok(oai) = translate_anthropic_to_openai_request(&body) {
                body = oai;
            }
            "/v1/chat/completions"
        } else {
            path_and_query
        };
        let url = slot
            .upstream
            .as_deref()
            .map(|base| join_base_path(base, effective_path))
            .unwrap_or_else(|| default_target_url.to_owned());
        let slot_headers = build_slot_headers(headers, slot.api_key.as_deref(), bridge);
        let req = apply_proxy_headers(client.request(method.clone(), &url), &slot_headers, body.len())
            .body(body);

        match req.send().await {
            Ok(res) => {
                let code = res.status().as_u16();
                if matches!(code, 402 | 429 | 500 | 502 | 503) && hop + 1 < ordered.len().min(4) {
                    let retry_after = parse_retry_after(res.headers());
                    tracker.mark_cooldown(&slot.slot_key, retry_after);
                    if let Some(ref acct_id) = slot.account_id {
                        quotas.mark_cooldown(acct_id, code, retry_after.unwrap_or(DEFAULT_FAILOVER_COOLDOWN_SECS));
                    }
                    eprintln!("[aphelion] failover {} -> {} (HTTP {code})", slot.slot_key, ordered[hop + 1].slot_key);
                    did_failover = true;
                    continue;
                }
                return Some(DispatchResult {
                    response: res,
                    model: slot.model.clone(),
                    account_id: slot.account_id.clone(),
                    did_failover,
                    bridged_oai_to_anthropic: bridge,
                });
            }
            Err(_) => {
                tracker.mark_cooldown(&slot.slot_key, Some(15));
                if let Some(ref acct_id) = slot.account_id {
                    quotas.mark_cooldown(acct_id, 502, 15);
                }
                if hop + 1 < ordered.len().min(4) {
                    eprintln!("[aphelion] failover {} -> {} (connect error)", slot.slot_key, ordered[hop + 1].slot_key);
                    did_failover = true;
                    continue;
                }
                return None;
            }
        }
    }
    None
}
