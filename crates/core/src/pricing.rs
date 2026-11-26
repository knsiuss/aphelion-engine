use serde::{Deserialize, Serialize};

/// Per-model pricing in USD per 1M tokens (input, output, cache read/write multipliers).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct ModelPricing {
    pub input_per_m: f64,
    pub output_per_m: f64,
    pub cache_read_mult: f64,
    pub cache_write_mult: f64,
}

impl ModelPricing {
    pub const fn new(
        input_per_m: f64,
        output_per_m: f64,
        cache_read_mult: f64,
        cache_write_mult: f64,
    ) -> Self {
        Self {
            input_per_m,
            output_per_m,
            cache_read_mult,
            cache_write_mult,
        }
    }
}

/// Resolves realistic USD/1M pricing for `(provider, model)` instead of flat Claude Sonnet rates.
/// Local/Ollama models return `$0.00`; Flash/DeepSeek/Gemini/GPT/Claude map to published tiers.
pub fn resolve_model_pricing(provider: &str, model: &str) -> ModelPricing {
    let p = provider.trim().to_ascii_lowercase();
    let m = model.trim().to_ascii_lowercase();

    if p == "ollama" || p == "local" || m.starts_with("ollama/") || m.contains(":latest") {
        return ModelPricing::new(0.0, 0.0, 0.0, 1.0);
    }

    if m.contains("claude") {
        if m.contains("opus") {
            return ModelPricing::new(15.0, 75.0, 0.10, 1.25);
        }
        if m.contains("haiku") {
            return ModelPricing::new(0.80, 4.0, 0.10, 1.25);
        }
        return ModelPricing::new(3.0, 15.0, 0.10, 1.25);
    }

    if m.contains("deepseek") {
        if m.contains("flash") || m.contains("lite") {
            return ModelPricing::new(0.14, 0.28, 0.25, 1.0);
        }
        if m.contains("reasoner") || m.contains("-r1") {
            return ModelPricing::new(0.55, 2.19, 0.25, 1.0);
        }
        return ModelPricing::new(0.27, 1.10, 0.25, 1.0);
    }

    if m.contains("ernie") || m.contains("flash-lite") {
        return ModelPricing::new(0.10, 0.40, 0.25, 1.0);
    }

    if m.contains("gemini") {
        if m.contains("pro") {
            return ModelPricing::new(1.25, 10.0, 0.25, 1.0);
        }
        return ModelPricing::new(0.15, 0.60, 0.25, 1.0);
    }

    if m.contains("gpt-4o-mini") || m.contains("gpt-4.1-mini") || m.contains("gpt-4.1-nano") {
        return ModelPricing::new(0.15, 0.60, 0.50, 1.0);
    }
    if m.contains("gpt-4") || m.contains("gpt-5") {
        return ModelPricing::new(2.50, 10.0, 0.50, 1.0);
    }
    if m.starts_with("o3-mini") || m.starts_with("o4-mini") || m.starts_with("o1-mini") {
        return ModelPricing::new(1.10, 4.40, 0.50, 1.0);
    }
    if m.starts_with("o1") || m.starts_with("o3") {
        return ModelPricing::new(10.0, 40.0, 0.50, 1.0);
    }

    if m.contains("mistral") || m.contains("codestral") {
        return ModelPricing::new(0.30, 0.90, 0.50, 1.0);
    }
    if m.contains("qwen") || m.contains("kimi") || m.contains("moonshot") || m.contains("glm") {
        return ModelPricing::new(0.40, 1.20, 0.25, 1.0);
    }
    if m.contains("flash") || m.contains("mini") || m.contains("small") {
        return ModelPricing::new(0.15, 0.60, 0.25, 1.0);
    }

    ModelPricing::new(1.0, 3.0, 0.25, 1.0)
}

#[cfg(test)]
mod tests {
    use super::resolve_model_pricing;

    #[test]
    fn resolves_expected_pricing_tiers() {
        assert_eq!(resolve_model_pricing("ollama", "qwen2.5-coder:latest").input_per_m, 0.0);
        assert_eq!(resolve_model_pricing("anthropic", "claude-opus-4-1").output_per_m, 75.0);
        assert_eq!(resolve_model_pricing("anthropic", "claude-haiku-4-5").input_per_m, 0.80);
        assert_eq!(resolve_model_pricing("anthropic", "claude-sonnet-4-5").input_per_m, 3.0);
        assert_eq!(resolve_model_pricing("litellm", "mistral-deepseek-1017").input_per_m, 0.27);
        assert_eq!(resolve_model_pricing("litellm", "deepseek-v4-1-flash-1017").input_per_m, 0.14);
        assert_eq!(resolve_model_pricing("litellm", "ernie-5-5-flash").input_per_m, 0.10);
        assert_eq!(resolve_model_pricing("openai", "gpt-4o").input_per_m, 2.50);
        assert_eq!(resolve_model_pricing("openai", "gpt-4o-mini").input_per_m, 0.15);
        assert_eq!(resolve_model_pricing("custom", "some-unknown-model").input_per_m, 1.0);
    }
}
