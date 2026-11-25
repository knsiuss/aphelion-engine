//! Pure Sans-I/O Flight-Recorder domain types, canonical provider normalization,
//! duration parsing, and asymmetric net-savings calculation.

use serde::{Deserialize, Serialize};

/// Canonicalizes any raw provider identifier into a stable lowercase label,
/// preserving dynamic hostname labels (e.g., `moonshot.cn`, `local:8000`).
pub fn normalize_provider_label(raw: &str) -> String {
    let trimmed = raw.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        return "unknown".to_owned();
    }
    match trimmed.as_str() {
        "google" => "gemini".to_owned(),
        "x.ai" => "xai".to_owned(),
        "hf" => "huggingface".to_owned(),
        _ => trimmed,
    }
}

/// Canonicalizes known provider identifiers into a static lowercase label.
pub fn canonical_provider(raw: &str) -> &'static str {
    match raw.trim().to_ascii_lowercase().as_str() {
        "openrouter" => "openrouter",
        "mistral" => "mistral",
        "anthropic" => "anthropic",
        "openai" => "openai",
        "gemini" | "google" => "gemini",
        "deepseek" => "deepseek",
        "groq" => "groq",
        "xai" | "x.ai" => "xai",
        "fireworks" => "fireworks",
        "huggingface" | "hf" => "huggingface",
        "cerebras" => "cerebras",
        "perplexity" => "perplexity",
        "together" => "together",
        "opencode" => "opencode",
        "ollama" => "ollama",
        "custom" => "custom",
        _ => "unknown",
    }
}

/// Parses a human duration filter like `"30m"`, `"1h"`, `"24h"`, or `"7d"` into milliseconds.
pub fn parse_duration_ms(spec: &str) -> Option<i64> {
    let trimmed = spec.trim().to_ascii_lowercase();
    if trimmed.len() < 2 {
        return None;
    }
    let (num_str, unit) = trimmed.split_at(trimmed.len() - 1);
    let val = num_str.parse::<i64>().ok()?;
    if val <= 0 {
        return None;
    }
    let mult = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return None,
    };
    val.checked_mul(mult)
}

/// Computes asymmetric net USD saved for a single turn (cached 0.1x vs uncached 1.0x).
#[allow(clippy::too_many_arguments)]
pub fn compute_turn_net_saved_usd(
    turn_index: usize,
    raw_tokens: usize,
    opt_tokens: usize,
    uncached_input_tokens: usize,
    cache_read_tokens: usize,
    cache_creation_tokens: usize,
    output_tokens: usize,
    c_eff_usd: f64,
) -> f64 {
    let uncached_rate = 3.0 / 1_000_000.0;
    let cache_read_rate = 0.10 * uncached_rate;
    let cache_write_rate = 1.25 * uncached_rate;
    let output_rate = 15.0 / 1_000_000.0;

    let actual_uncached = if uncached_input_tokens > 0 || cache_read_tokens > 0 {
        uncached_input_tokens
    } else {
        opt_tokens
    };
    let actual_turn_cost = (actual_uncached as f64) * uncached_rate
        + (cache_read_tokens as f64) * cache_read_rate
        + (cache_creation_tokens as f64) * cache_write_rate
        + (output_tokens as f64) * output_rate;
    let actual = if c_eff_usd > 0.0 && uncached_input_tokens == 0 {
        c_eff_usd
    } else {
        actual_turn_cost
    };

    let (base_cached, base_uncached) = if turn_index > 1 {
        let c = ((raw_tokens as f64) * 0.90) as usize;
        (c, raw_tokens.saturating_sub(c))
    } else {
        (0, raw_tokens)
    };
    let baseline = (base_uncached as f64) * uncached_rate
        + (base_cached as f64) * cache_read_rate
        + (output_tokens as f64) * output_rate;
    baseline - actual
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditFilter {
    pub provider: Option<String>,
    pub model_substring: Option<String>,
    pub since_timestamp_ms: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderModelStats {
    pub provider: String,
    pub model: String,
    pub requests: usize,
    pub total_ttft_ms: u64,
    pub cached_read_tokens: usize,
    pub cache_write_tokens: usize,
    pub uncached_input_tokens: usize,
    pub output_tokens: usize,
    pub reasoning_tokens: usize,
    pub ingress_tokens_saved: usize,
    pub net_saved_usd: f64,
}

impl ProviderModelStats {
    pub fn avg_ttft_ms(&self) -> u64 {
        if self.requests == 0 {
            0
        } else {
            self.total_ttft_ms / (self.requests as u64)
        }
    }

    pub fn cache_hit_pct(&self) -> f64 {
        let denom = self.cached_read_tokens + self.uncached_input_tokens + self.cache_write_tokens;
        if denom == 0 {
            0.0
        } else {
            (self.cached_read_tokens as f64 * 100.0) / (denom as f64)
        }
    }

    pub fn total_tokens(&self) -> usize {
        self.cached_read_tokens
            + self.cache_write_tokens
            + self.uncached_input_tokens
            + self.output_tokens
    }
}
