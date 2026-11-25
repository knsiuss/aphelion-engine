//! Centralized Sans-I/O operational defaults for `aphelion-core`.
//! Runtime callers override these via `RequestOptions` (wired from `APHELION_*` env vars / CLI).

/// Default high watermark (in BPE tokens) before amortized macro-compaction triggers.
pub const DEFAULT_COMPACTION_THRESHOLD_TOKENS: usize = 120_000;
/// Default number of recent tail messages locked verbatim during macro-compaction.
pub const DEFAULT_LOCKED_TAIL_MESSAGES: usize = 6;
/// Default projected future turns used in the 10:1 asymmetric break-even inequality.
pub const DEFAULT_PROJECTED_REMAINING_TURNS: usize = 12;
/// Minimum rolling cache-hit ratio on turns >= 3 before locking into `FrozenPrefixPass`.
pub const DEFAULT_CACHE_DROP_BREAKER_THRESHOLD: f64 = 0.70;
/// Maximum LRU sessions retained in an in-memory `PrefixLedger`.
pub const DEFAULT_MAX_LEDGER_SESSIONS: usize = 512;
/// Default local compiled-SLM sidecar endpoint (overridable via `APHELION_SLM_ENDPOINT`).
pub const DEFAULT_SLM_ENDPOINT: &str = "http://127.0.0.1:8081/v1";
/// Default consecutive block repetitions before SSE loop breaker terminates runaway output.
pub const DEFAULT_SSE_LOOP_MIN_REPS: usize = 3;
/// Max BPE context tokens eligible for cache-aware `Tier::Fast` routing (protects warm >12k caches).
pub const DEFAULT_FAST_MAX_CONTEXT_TOKENS: usize = 12_000;
/// Default cooldown duration (seconds) after upstream HTTP 429 / 502 / 503 failover.
pub const DEFAULT_FAILOVER_COOLDOWN_SECS: u64 = 60;
