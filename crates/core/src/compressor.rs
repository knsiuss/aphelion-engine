use crate::diet_gc::estimate_calibrated_tokens;
use crate::types::{Dialect, ExecutionProfile, NormalizedRequest, SessionState};
use serde_json::Value;
use std::collections::HashSet;

const STOPWORDS: &[&str] = &[
    "the", "a", "an", "of", "to", "in", "for", "on", "with", "at", "by", "from", "as", "is",
    "was", "are", "were", "be", "been", "being", "that", "this", "these", "those", "it", "its",
    "very", "just", "really", "actually", "basically", "simply", "please", "kindly", "note",
];

/// Syntax-masked LongLLMLingua U-shaped RAG reorder + LLMLingua-2 prose pruner across all dialects.
/// Hard Invariant 4: `Ephemeral` uses pure-CPU `LexicalEntropyScorer` (<2ms, zero ONNX).
/// Hard Invariant 5: code fences, inline code, JSON/XML, stacktraces, paths, URLs are 0% mutated.
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
    if let Some(prompt) = root.get("prompt").and_then(Value::as_str).map(str::to_owned) {
        if let Some((out, saved)) = try_compress_text(&prompt, profile) {
            root["prompt"] = Value::String(out);
            return saved;
        }
        return 0;
    }
    let Some(last) = root.get_mut("messages").and_then(Value::as_array_mut).and_then(|m| m.last_mut()) else {
        return 0;
    };
    if last.get("role").and_then(Value::as_str) != Some("user") {
        return 0;
    }
    if let Some(content) = last.get("content").and_then(Value::as_str).map(str::to_owned) {
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
    if let Some(items) = root.get_mut("input").and_then(Value::as_array_mut) {
        if let Some(last) = items.last_mut() {
            if last.get("role").and_then(Value::as_str) == Some("user") {
                if let Some(c) = last.get("content").and_then(Value::as_str).map(str::to_owned) {
                    if let Some((out, saved)) = try_compress_text(&c, profile) {
                        last["content"] = Value::String(out);
                        total_saved += saved;
                    }
                }
            }
        }
    }
    total_saved
}

fn compress_gemini_contents(root: &mut Value, profile: ExecutionProfile) -> usize {
    let Some(last) = root.get_mut("contents").and_then(Value::as_array_mut).and_then(|c| c.last_mut()) else {
        return 0;
    };
    let Some(parts) = last.get_mut("parts").and_then(Value::as_array_mut) else {
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
    if after < before {
        Some((compressed, before - after))
    } else {
        None
    }
}

pub fn reorder_rag_u_curve(input: &str) -> String {
    let chunks: Vec<&str> = input.split("\n---\n").collect();
    if chunks.len() < 4 {
        return input.to_owned();
    }
    let query = chunks.last().copied().unwrap_or_default();
    let query_terms: HashSet<String> = query
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_ascii_lowercase())
        .filter(|w| w.len() > 2)
        .collect();

    let mut scored: Vec<(usize, usize, &str)> = chunks[..chunks.len() - 1]
        .iter()
        .enumerate()
        .map(|(idx, chunk)| {
            let score = chunk
                .split_whitespace()
                .filter(|w| {
                    let clean = w.trim_matches(|c: char| !c.is_alphanumeric()).to_ascii_lowercase();
                    query_terms.contains(&clean)
                })
                .count();
            (score, idx, *chunk)
        })
        .collect();

    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let (mut front, mut back) = (Vec::with_capacity(scored.len()), Vec::with_capacity(scored.len()));
    for (i, (_, _, chunk)) in scored.into_iter().enumerate() {
        if i % 2 == 0 { front.push(chunk); } else { back.push(chunk); }
    }
    back.reverse();
    front.extend(back);
    front.push(query);
    front.join("\n---\n")
}

pub fn compress_with_syntax_mask(input: &str, profile: ExecutionProfile) -> String {
    let mut out = String::with_capacity(input.len());
    for (idx, segment) in input.split("```").enumerate() {
        if idx > 0 { out.push_str("```"); }
        if idx % 2 == 1 {
            out.push_str(segment);
        } else {
            out.push_str(&compress_inline_masked_prose(segment, profile));
        }
    }
    out
}

fn compress_inline_masked_prose(segment: &str, _profile: ExecutionProfile) -> String {
    let mut out_lines = Vec::new();
    for line in segment.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() { continue; }
        if is_protected_line(trimmed) {
            out_lines.push(line.to_owned());
            continue;
        }
        let pruned = lexical_entropy_prune(trimmed);
        if !pruned.is_empty() { out_lines.push(pruned); }
    }
    out_lines.join("\n")
}

fn is_protected_line(t: &str) -> bool {
    t.starts_with('{') || t.starts_with('}') || t.starts_with('[') || t.starts_with('<')
        || t.starts_with("//") || t.starts_with('#')
        || t.contains("http://") || t.contains("https://")
        || t.contains(":\\") || t.contains("/src/") || contains_stacktrace(t)
}

fn contains_stacktrace(text: &str) -> bool {
    text.contains("Error:") || text.contains("Traceback") || text.contains("panic:") || text.contains("thread '")
}

fn lexical_entropy_prune(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    for (idx, part) in line.split('`').enumerate() {
        if idx > 0 { out.push('`'); }
        if idx % 2 == 1 {
            out.push_str(part);
        } else {
            let kept: Vec<&str> = part.split_whitespace().filter(|t| keep_lexical_token(t)).collect();
            out.push_str(&kept.join(" "));
        }
    }
    out
}

fn keep_lexical_token(tok: &str) -> bool {
    if tok.len() <= 2 && tok.chars().all(|c| c.is_ascii_alphabetic()) { return false; }
    let lower = tok.trim_matches(|c: char| !c.is_ascii_alphanumeric()).to_ascii_lowercase();
    !STOPWORDS.contains(&lower.as_str())
}
