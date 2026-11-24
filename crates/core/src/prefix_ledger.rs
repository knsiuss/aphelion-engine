use crate::diet_gc::estimate_calibrated_tokens;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};

/// High watermark for amortized macro-compaction (120,000 BPE tokens).
pub const MACRO_COMPACTION_WATERMARK_TOKENS: usize = 120_000;
/// Minimum rolling cache-hit ratio on turns >= 3 before tripping `FrozenPrefixPass`.
pub const CACHE_DROP_BREAKER_THRESHOLD: f64 = 0.70;
const MAX_LEDGER_SESSIONS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LedgerKey {
    pub session_root_hash: u128,
    pub model_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FrozenSessionRecord {
    pub frozen_prefix_bytes: Vec<u8>,
    pub frozen_messages: Vec<Value>,
    pub frozen_file_hashes: HashMap<String, u64>,
    pub committed_len: usize,
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

    /// Records real upstream usage and trips the Cache-Drop Circuit Breaker if
    /// turns >= 3 and rolling `cache_hit_ratio < 0.70` on a cache-capable upstream.
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

/// Deterministic in-memory LRU `PrefixLedger` keyed by `(session_root_hash, model_id)`.
/// Enforces the Append-Only History Lock: once messages `0..committed_len` are dispatched
/// upstream, their canonical bytes are strictly immutable on all subsequent turns.
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
        LedgerKey {
            session_root_hash: session_id.unwrap_or(0),
            model_id: model.to_owned(),
        }
    }
}

/// Replays strictly frozen historical messages `0..k` verbatim into `messages`,
/// returning `ingress_start = k` (the first newly appended message eligible for optimization).
pub fn replay_frozen_history(messages: &mut [Value], record: &FrozenSessionRecord) -> usize {
    let k = record.committed_len.min(record.frozen_messages.len());
    if k == 0 || messages.len() < k {
        return 0;
    }
    for (slot, frozen) in messages[..k]
        .iter_mut()
        .zip(record.frozen_messages[..k].iter())
    {
        *slot = frozen.clone();
    }
    k
}

/// Commits all outbound messages `0..n` into `FrozenSessionRecord` so turn `t+1`
/// replays their exact canonical post-optimization bytes with 100% prefix stability.
pub fn commit_outbound_messages(messages: &[Value], record: &mut FrozenSessionRecord) {
    record.frozen_messages = messages.to_vec();
    record.committed_len = messages.len();
}

/// Evaluates whether a macro-compaction checkpoint is economically amortized:
/// requires total context >= 120,000 BPE tokens AND `re_prefill_tokens * 10 < projected_saved_reads`.
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
