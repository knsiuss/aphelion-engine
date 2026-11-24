use crate::types::UpstreamUsage;
use serde_json::Value;

/// Extracts real upstream usage and cache-hit metrics across OpenAI Chat, DeepSeek
/// (`prompt_cache_hit_tokens` / `prompt_cache_miss_tokens`), OpenAI Responses API,
/// Anthropic (`cache_read_input_tokens` / `cache_creation_input_tokens`), Gemini, and Ollama.
pub fn extract_usage_universal(root: &Value) -> Option<UpstreamUsage> {
    if let Some(meta) = root.get("usageMetadata") {
        let input = meta
            .get("promptTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let cached = meta
            .get("cachedContentTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        return Some(UpstreamUsage {
            input_tokens: input,
            uncached_input_tokens: input.saturating_sub(cached),
            output_tokens: meta
                .get("candidatesTokenCount")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize,
            cached_tokens: cached,
            cache_creation_tokens: 0,
            reasoning_tokens: meta
                .get("thoughtsTokenCount")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize,
        });
    }

    if root.get("prompt_eval_count").is_some() || root.get("eval_count").is_some() {
        let input = root
            .get("prompt_eval_count")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        return Some(UpstreamUsage {
            input_tokens: input,
            uncached_input_tokens: input,
            output_tokens: root.get("eval_count").and_then(Value::as_u64).unwrap_or(0) as usize,
            ..UpstreamUsage::default()
        });
    }

    let u = root
        .pointer("/response/usage")
        .or_else(|| root.get("usage"))?;

    if let Some(pt) = u.get("prompt_tokens").and_then(Value::as_u64) {
        let input = pt as usize;
        let cached = u
            .pointer("/prompt_tokens_details/cached_tokens")
            .or_else(|| u.get("prompt_cache_hit_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let uncached = u
            .get("prompt_cache_miss_tokens")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .unwrap_or_else(|| input.saturating_sub(cached));
        return Some(UpstreamUsage {
            input_tokens: input,
            uncached_input_tokens: uncached,
            output_tokens: u
                .get("completion_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize,
            cached_tokens: cached,
            cache_creation_tokens: 0,
            reasoning_tokens: u
                .pointer("/completion_tokens_details/reasoning_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize,
        });
    }

    if u.get("input_tokens").is_some() || u.get("output_tokens").is_some() {
        let input = u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0) as usize;
        let cached = u
            .get("cache_read_input_tokens")
            .or_else(|| u.pointer("/input_tokens_details/cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let cache_create = u
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let is_anthropic = u.get("cache_read_input_tokens").is_some()
            || u.get("cache_creation_input_tokens").is_some();
        let (total_in, uncached) = if is_anthropic {
            (
                input.saturating_add(cached).saturating_add(cache_create),
                input,
            )
        } else {
            (input, input.saturating_sub(cached))
        };
        return Some(UpstreamUsage {
            input_tokens: total_in,
            uncached_input_tokens: uncached,
            output_tokens: u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0) as usize,
            cached_tokens: cached,
            cache_creation_tokens: cache_create,
            reasoning_tokens: u
                .pointer("/output_tokens_details/reasoning_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize,
        });
    }

    None
}
