pub use crate::compressor_rules::{
    compress_with_syntax_mask, contains_stacktrace, reorder_rag_u_curve,
};
use crate::diet_gc::estimate_calibrated_tokens;
use crate::types::{Dialect, ExecutionProfile, NormalizedRequest, SessionState};
use serde_json::Value;

/// Syntax-masked LongLLMLingua U-shaped RAG reorder + LLMLingua-2 prose pruner across all dialects.
pub fn compress_masked_prose(
    ir: &mut NormalizedRequest,
    profile: ExecutionProfile,
    state: &SessionState,
) {
    if state.sacred_cache_boundary > 0 && !ir.is_turn_zero {
        return;
    }
    let saved = match ir.dialect {
        Dialect::OpenAiChat | Dialect::AnthropicMessages | Dialect::OllamaNative => {
            compress_chat_or_prompt(&mut ir.root, profile)
        }
        Dialect::OpenAiResponses => compress_responses_input(&mut ir.root, profile),
        Dialect::GeminiGenerate => compress_gemini_contents(&mut ir.root, profile),
        Dialect::Passthrough => 0,
    };
    ir.stage_stats.prose_pruned_tokens += saved;
}

fn compress_chat_or_prompt(root: &mut Value, profile: ExecutionProfile) -> usize {
    if let Some(prompt) = root
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::to_owned)
    {
        if let Some((out, saved)) = try_compress_text(&prompt, profile) {
            root["prompt"] = Value::String(out);
            return saved;
        }
        return 0;
    }
    let Some(last) = root
        .get_mut("messages")
        .and_then(Value::as_array_mut)
        .and_then(|m| m.last_mut())
    else {
        return 0;
    };
    if last.get("role").and_then(Value::as_str) != Some("user") {
        return 0;
    }
    if let Some(content) = last
        .get("content")
        .and_then(Value::as_str)
        .map(str::to_owned)
    {
        if let Some((out, saved)) = try_compress_text(&content, profile) {
            last["content"] = Value::String(out);
            return saved;
        }
        return 0;
    }
    let mut total_saved = 0usize;
    if let Some(blocks) = last.get_mut("content").and_then(Value::as_array_mut) {
        for block in blocks {
            if block.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(t) = block.get("text").and_then(Value::as_str).map(str::to_owned) {
                    if let Some((out, saved)) = try_compress_text(&t, profile) {
                        block["text"] = Value::String(out);
                        total_saved += saved;
                    }
                }
            }
        }
    }
    total_saved
}

fn compress_responses_input(root: &mut Value, profile: ExecutionProfile) -> usize {
    if let Some(s) = root.get("input").and_then(Value::as_str).map(str::to_owned) {
        if let Some((out, saved)) = try_compress_text(&s, profile) {
            root["input"] = Value::String(out);
            return saved;
        }
        return 0;
    }
    let mut total_saved = 0usize;
    if let Some(last) = root
        .get_mut("input")
        .and_then(Value::as_array_mut)
        .and_then(|i| i.last_mut())
    {
        if last.get("role").and_then(Value::as_str) == Some("user") {
            if let Some(c) = last
                .get("content")
                .and_then(Value::as_str)
                .map(str::to_owned)
            {
                if let Some((out, saved)) = try_compress_text(&c, profile) {
                    last["content"] = Value::String(out);
                    total_saved += saved;
                }
            }
        }
    }
    total_saved
}

fn compress_gemini_contents(root: &mut Value, profile: ExecutionProfile) -> usize {
    let Some(parts) = root
        .get_mut("contents")
        .and_then(Value::as_array_mut)
        .and_then(|c| c.last_mut())
        .and_then(|l| l.get_mut("parts"))
        .and_then(Value::as_array_mut)
    else {
        return 0;
    };
    let mut total_saved = 0usize;
    for part in parts {
        if part.get("functionCall").is_some() || part.get("functionResponse").is_some() {
            continue;
        }
        if let Some(t) = part.get("text").and_then(Value::as_str).map(str::to_owned) {
            if let Some((out, saved)) = try_compress_text(&t, profile) {
                part["text"] = Value::String(out);
                total_saved += saved;
            }
        }
    }
    total_saved
}

fn try_compress_text(content: &str, profile: ExecutionProfile) -> Option<(String, usize)> {
    if contains_stacktrace(content) {
        return None;
    }
    let before = estimate_calibrated_tokens(content);
    let reordered = reorder_rag_u_curve(content);
    let compressed = compress_with_syntax_mask(&reordered, profile);
    let after = estimate_calibrated_tokens(&compressed);
    (after < before).then(|| (compressed, before - after))
}
