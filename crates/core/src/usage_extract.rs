use crate::types::UpstreamUsage;
use serde_json::Value;

/// Extracts real upstream usage and cache-hit metrics across OpenAI Chat/DeepSeek/vLLM,
/// OpenAI Responses API, Anthropic Messages, Google Gemini, and Ollama NDJSON.
pub fn extract_usage_universal(root: &Value) -> Option<UpstreamUsage> {
    // 1. Google Gemini (`usageMetadata`)
    if let Some(meta) = root.get("usageMetadata") {
        return Some(UpstreamUsage {
            input_tokens: meta.get("promptTokenCount").and_then(Value::as_u64).unwrap_or(0) as usize,
            output_tokens: meta.get("candidatesTokenCount").and_then(Value::as_u64).unwrap_or(0) as usize,
            cached_tokens: meta.get("cachedContentTokenCount").and_then(Value::as_u64).unwrap_or(0) as usize,
            cache_creation_tokens: 0,
            reasoning_tokens: meta.get("thoughtsTokenCount").and_then(Value::as_u64).unwrap_or(0) as usize,
        });
    }

    // 2. Ollama NDJSON (`prompt_eval_count` / `eval_count`)
    if root.get("prompt_eval_count").is_some() || root.get("eval_count").is_some() {
        return Some(UpstreamUsage {
            input_tokens: root.get("prompt_eval_count").and_then(Value::as_u64).unwrap_or(0) as usize,
            output_tokens: root.get("eval_count").and_then(Value::as_u64).unwrap_or(0) as usize,
            ..UpstreamUsage::default()
        });
    }

    // 3. OpenAI Responses API (`response.completed` wrapper or direct `usage`)
    let u = root
        .pointer("/response/usage")
        .or_else(|| root.get("usage"))?;

    // OpenAI Chat / DeepSeek / OpenRouter / vLLM
    if let Some(pt) = u.get("prompt_tokens").and_then(Value::as_u64) {
        return Some(UpstreamUsage {
            input_tokens: pt as usize,
            output_tokens: u.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0) as usize,
            cached_tokens: u
                .pointer("/prompt_tokens_details/cached_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize,
            cache_creation_tokens: 0,
            reasoning_tokens: u
                .pointer("/completion_tokens_details/reasoning_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize,
        });
    }

    // Anthropic Messages OR OpenAI Responses API (`input_tokens` / `output_tokens`)
    if u.get("input_tokens").is_some() || u.get("output_tokens").is_some() {
        let cached = u
            .get("cache_read_input_tokens")
            .or_else(|| u.pointer("/input_tokens_details/cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        return Some(UpstreamUsage {
            input_tokens: u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0) as usize,
            output_tokens: u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0) as usize,
            cached_tokens: cached,
            cache_creation_tokens: u
                .get("cache_creation_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize,
            reasoning_tokens: u
                .pointer("/output_tokens_details/reasoning_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize,
        });
    }

    None
}
