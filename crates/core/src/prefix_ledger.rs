use crate::defaults::{
    DEFAULT_CACHE_DROP_BREAKER_THRESHOLD, DEFAULT_COMPACTION_THRESHOLD_TOKENS,
    DEFAULT_MAX_LEDGER_SESSIONS,
};
use crate::diet_gc::estimate_calibrated_tokens;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};

/// High watermark for amortized macro-compaction (from centralized `defaults`).
pub const MACRO_COMPACTION_WATERMARK_TOKENS: usize = DEFAULT_COMPACTION_THRESHOLD_TOKENS;
/// Minimum rolling cache-hit ratio on turns >= 3 before tripping `FrozenPrefixPass`.
pub const CACHE_DROP_BREAKER_THRESHOLD: f64 = DEFAULT_CACHE_DROP_BREAKER_THRESHOLD;
const MAX_LEDGER_SESSIONS: usize = DEFAULT_MAX_LEDGER_SESSIONS;

/// Cryptographically tenant-isolated ledger key: `(tenant_hash, session_root_hash, provider_id, model_id)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LedgerKey {
    pub tenant_hash: u64,
    pub session_root_hash: u128,
    pub provider_id: String,
    pub model_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FrozenSessionRecord {
    pub frozen_prefix_bytes: Vec<u8>,
    pub frozen_messages: Vec<Value>,
    pub frozen_file_hashes: HashMap<String, u64>,
    pub committed_len: usize,
    pub compaction_epoch: u32,
    #[serde(default)]
    pub compacted_dropped_count: usize,
    pub rolling_cached_read: usize,
    pub rolling_total_input: usize,
    pub usage_turns: usize,
    pub upstream_supports_cache: bool,
    pub cache_drop_breaker_tripped: bool,
}

impl FrozenSessionRecord {
    pub fn cache_hit_ratio(&self) -> f64 {
        if self.rolling_total_input == 0 {
            return 1.0;
        }
        self.rolling_cached_read as f64 / self.rolling_total_input as f64
    }

    pub fn record_usage(&mut self, input_tokens: usize, cached_read: usize, cache_write: usize) {
        if input_tokens == 0 {
            return;
        }
        self.usage_turns += 1;
        if cached_read > 0 || cache_write > 0 {
            self.upstream_supports_cache = true;
        }
        self.rolling_cached_read += cached_read;
        self.rolling_total_input += input_tokens;

        if self.upstream_supports_cache
            && self.usage_turns >= 3
            && self.cache_hit_ratio() < CACHE_DROP_BREAKER_THRESHOLD
        {
            self.cache_drop_breaker_tripped = true;
        }
    }
}

/// Deterministic in-memory LRU `PrefixLedger` enforcing Append-Only Prefix Invariance
/// and zero cross-tenant state sharing.
#[derive(Debug, Clone, Default)]
pub struct PrefixLedger {
    entries: HashMap<LedgerKey, FrozenSessionRecord>,
    lru_order: VecDeque<LedgerKey>,
}

impl PrefixLedger {
    pub fn get(&self, key: &LedgerKey) -> Option<&FrozenSessionRecord> {
        self.entries.get(key)
    }

    pub fn get_or_insert_mut(&mut self, key: LedgerKey) -> &mut FrozenSessionRecord {
        if !self.entries.contains_key(&key) {
            if self.lru_order.len() >= MAX_LEDGER_SESSIONS {
                if let Some(oldest) = self.lru_order.pop_front() {
                    self.entries.remove(&oldest);
                }
            }
            self.lru_order.push_back(key.clone());
            self.entries
                .insert(key.clone(), FrozenSessionRecord::default());
        }
        self.entries.entry(key).or_default()
    }

    pub fn restore_record(&mut self, key: LedgerKey, record: FrozenSessionRecord) {
        if !self.entries.contains_key(&key) {
            self.lru_order.push_back(key.clone());
        }
        self.entries.insert(key, record);
    }

    pub fn active_key(session_id: Option<u128>, model: &str) -> LedgerKey {
        Self::tenant_key(0, session_id, "default", model)
    }

    pub fn tenant_key(
        tenant_hash: u64,
        session_id: Option<u128>,
        provider_id: &str,
        model: &str,
    ) -> LedgerKey {
        LedgerKey {
            tenant_hash,
            session_root_hash: session_id.unwrap_or(0),
            provider_id: if provider_id.is_empty() {
                "default".to_owned()
            } else {
                provider_id.to_owned()
            },
            model_id: model.to_owned(),
        }
    }
}

/// Replays strictly frozen historical messages `0..k` verbatim into `messages`,
/// verifying role and user-prompt prefix continuity so client history edits/rewinds
/// fork cleanly at the divergence index instead of overwriting user edits.
fn has_checkpoint_msg(msgs: &[Value], record: &FrozenSessionRecord) -> bool {
    record.compaction_epoch > 0
        || msgs
            .get(1)
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str)
            .is_some_and(|s| s.starts_with("[APHELION_CHECKPOINT"))
}

pub fn replay_frozen_history(messages: &mut Vec<Value>, record: &FrozenSessionRecord) -> usize {
    let k = record.committed_len.min(record.frozen_messages.len());
    if k == 0 || messages.is_empty() {
        return 0;
    }
    if has_checkpoint_msg(&record.frozen_messages, record) {
        let raw_len = k.saturating_add(record.compacted_dropped_count);
        let client_uncompacted = record.compacted_dropped_count > 0
            && messages.len() >= raw_len
            && !messages.get(1).and_then(|m| m.get("content")).and_then(Value::as_str)
                .is_some_and(|s| s.starts_with("[APHELION_CHECKPOINT"));
        if client_uncompacted {
            let replayed: Vec<Value> = record.frozen_messages[..k].iter().enumerate().map(|(i, frozen)| {
                let mut merged = frozen.clone();
                if i == 0 { if let Some(inc0) = messages.first() { sync_incoming_cache_control(inc0, &mut merged); } }
                merged
            }).collect();
            messages.splice(0..raw_len, replayed);
            return k;
        }
        if messages.len() >= k {
            for (slot, frozen) in messages[..k].iter_mut().zip(record.frozen_messages[..k].iter()) {
                let mut merged = frozen.clone();
                sync_incoming_cache_control(slot, &mut merged);
                *slot = merged;
            }
            return k;
        }
    }

    let limit = k.min(messages.len());
    let mut matched = 0usize;
    for (i, (inc, frz)) in messages[..limit].iter().zip(record.frozen_messages[..limit].iter()).enumerate() {
        if !message_slot_compatible(inc, frz) { break; }
        matched = i + 1;
    }
    for (slot, frozen) in messages[..matched].iter_mut().zip(record.frozen_messages[..matched].iter()) {
        let mut merged = frozen.clone();
        sync_incoming_cache_control(slot, &mut merged);
        *slot = merged;
    }
    matched
}

/// Mirrors incoming `cache_control` markers onto the replayed frozen message (preserving
/// newly placed sliding breakpoints and removing retired ones so total stays <= 4).
fn sync_incoming_cache_control(incoming: &Value, target: &mut Value) {
    match (incoming, target) {
        (Value::Object(in_map), Value::Object(tgt_map)) => {
            match in_map.get("cache_control") {
                Some(cc) if !cc.is_null() => { tgt_map.insert("cache_control".to_owned(), cc.clone()); }
                _ => { tgt_map.remove("cache_control"); }
            }
            if let (Some(in_c), Some(tgt_c)) = (in_map.get("content"), tgt_map.get_mut("content")) {
                sync_incoming_cache_control(in_c, tgt_c);
            }
        }
        (Value::Array(in_arr), Value::Array(tgt_arr)) => {
            for (in_item, tgt_item) in in_arr.iter().zip(tgt_arr.iter_mut()) {
                sync_incoming_cache_control(in_item, tgt_item);
            }
        }
        _ => {}
    }
}

fn message_slot_compatible(incoming: &Value, frozen: &Value) -> bool {
    let inc_role = incoming.get("role").or_else(|| incoming.get("type")).and_then(Value::as_str);
    let frz_role = frozen.get("role").or_else(|| frozen.get("type")).and_then(Value::as_str);
    if inc_role != frz_role {
        return false;
    }
    if inc_role == Some("user") {
        if let (Some(ic), Some(fc)) = (
            incoming.get("content").and_then(Value::as_str),
            frozen.get("content").and_then(Value::as_str),
        ) {
            let ic_head = ic.trim().chars().take(16).collect::<String>();
            let fc_head = fc.trim().chars().take(16).collect::<String>();
            if !ic_head.is_empty() && !fc_head.is_empty() && ic_head != fc_head {
                return false;
            }
        }
    }
    true
}

pub fn commit_outbound_messages(messages: &[Value], record: &mut FrozenSessionRecord) {
    record.frozen_messages = messages.to_vec();
    record.committed_len = messages.len();
}

pub fn should_run_macro_compaction(
    messages: &[Value],
    stale_tokens: usize,
    projected_future_turns: usize,
    breaker_tripped: bool,
) -> bool {
    if breaker_tripped || stale_tokens == 0 {
        return false;
    }
    let total_tokens: usize = messages
        .iter()
        .map(|m| estimate_calibrated_tokens(&m.to_string()))
        .sum();
    if total_tokens < MACRO_COMPACTION_WATERMARK_TOKENS {
        return false;
    }
    let re_prefill_tokens = total_tokens.saturating_sub(stale_tokens);
    let projected_saved_reads = stale_tokens.saturating_mul(projected_future_turns);
    re_prefill_tokens.saturating_mul(10) < projected_saved_reads
}
