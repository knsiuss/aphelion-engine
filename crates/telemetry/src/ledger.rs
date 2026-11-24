use serde::{Deserialize, Serialize};

/// Pricing model in USD per 1M tokens for Effective Cost (`C_eff`) computation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PricingRates {
    pub input_per_m: f64,
    pub output_per_m: f64,
    pub cache_write_mult: f64,
    pub cache_read_mult: f64,
    pub local_slm_per_m: f64,
}

impl Default for PricingRates {
    fn default() -> Self {
        Self {
            input_per_m: 3.0,
            output_per_m: 15.0,
            cache_write_mult: 1.25,
            cache_read_mult: 0.10,
            local_slm_per_m: 0.03,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TurnTelemetry {
    pub raw_input_tokens: usize,
    pub opt_input_tokens: usize,
    pub cache_write_tokens: usize,
    pub cache_read_tokens: usize,
    pub think_output_tokens: usize,
    pub action_output_tokens: usize,
    pub slm_tokens: usize,
    pub hedge_count: usize,
    pub ast_nodes_before: usize,
    pub ast_nodes_after: usize,
}

impl TurnTelemetry {
    /// Effective Cost `C_eff` in USD accounting for 1.25x cache write, 0.1x cache read,
    /// `<think>` output tokens, action output tokens, and local compiled SLM tokens.
    pub fn effective_cost_usd(&self, rates: PricingRates) -> f64 {
        let uncached = self
            .opt_input_tokens
            .saturating_sub(self.cache_write_tokens.saturating_add(self.cache_read_tokens));
        let in_cost = ((uncached as f64)
            + (self.cache_write_tokens as f64) * rates.cache_write_mult
            + (self.cache_read_tokens as f64) * rates.cache_read_mult)
            * rates.input_per_m
            / 1_000_000.0;
        let out_cost = ((self.think_output_tokens + self.action_output_tokens) as f64)
            * rates.output_per_m
            / 1_000_000.0;
        let slm_cost = (self.slm_tokens as f64) * rates.local_slm_per_m / 1_000_000.0;
        in_cost + out_cost + slm_cost
    }

    /// Reasoning-to-Action ratio (`rho_RA`).
    pub fn reasoning_to_action_ratio(&self) -> f64 {
        if self.action_output_tokens == 0 {
            return self.think_output_tokens as f64;
        }
        self.think_output_tokens as f64 / self.action_output_tokens as f64
    }

    /// Self-Doubt Hedge Density (`delta_hedge`) per 100 `<think>` tokens.
    pub fn hedge_density(&self) -> f64 {
        if self.think_output_tokens == 0 {
            return 0.0;
        }
        (self.hedge_count as f64 * 100.0) / (self.think_output_tokens as f64)
    }

    /// AST Bloat Delta (`Delta_AST`).
    pub fn ast_bloat_delta(&self) -> isize {
        self.ast_nodes_after as isize - self.ast_nodes_before as isize
    }
}

/// Counts self-doubt hedge phrases inside a `<think>` trace.
pub fn count_self_doubt_hedges(think_text: &str) -> usize {
    let lower = think_text.to_ascii_lowercase();
    const HEDGES: &[&str] = &[
        "wait,",
        "let me double check",
        "actually,",
        "hmm",
        "hold on",
        "on second thought",
        "let me re-read",
    ];
    HEDGES.iter().map(|h| lower.matches(h).count()).sum()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionCostSummary {
    pub turns: usize,
    pub total_raw_tokens: usize,
    pub total_opt_tokens: usize,
    pub total_cache_read_tokens: usize,
    pub baseline_cost_usd: f64,
    pub optimized_cost_usd: f64,
}

impl SessionCostSummary {
    pub fn record(&mut self, turn: &TurnTelemetry, rates: PricingRates) {
        self.turns += 1;
        self.total_raw_tokens += turn.raw_input_tokens;
        self.total_opt_tokens += turn.opt_input_tokens;
        self.total_cache_read_tokens += turn.cache_read_tokens;
        self.baseline_cost_usd += (turn.raw_input_tokens as f64) * rates.input_per_m / 1_000_000.0
            + ((turn.think_output_tokens + turn.action_output_tokens) as f64) * rates.output_per_m
                / 1_000_000.0;
        self.optimized_cost_usd += turn.effective_cost_usd(rates);
    }

    pub fn token_savings_pct(&self) -> f64 {
        if self.total_raw_tokens == 0 {
            return 0.0;
        }
        let saved = self.total_raw_tokens.saturating_sub(self.total_opt_tokens) as f64;
        (saved * 100.0) / (self.total_raw_tokens as f64)
    }

    pub fn cost_savings_pct(&self) -> f64 {
        if self.baseline_cost_usd <= 0.0 {
            return 0.0;
        }
        ((self.baseline_cost_usd - self.optimized_cost_usd) * 100.0) / self.baseline_cost_usd
    }
}
