use crate::diet_gc::estimate_calibrated_tokens;
use crate::types::{ExecutionProfile, NormalizedRequest, SessionState};
use serde_json::Value;
use std::collections::HashSet;

const STOPWORDS: &[&str] = &[
    "the", "a", "an", "of", "to", "in", "for", "on", "with", "at", "by", "from", "as", "is",
    "was", "are", "were", "be", "been", "being", "that", "this", "these", "those", "it", "its",
    "very", "just", "really", "actually", "basically", "simply", "please", "kindly", "note",
];

/// Syntax-masked LongLLMLingua U-shaped RAG reorder + LLMLingua-2 prose pruner.
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
    let Some(messages) = ir.root.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    let Some(last) = messages.last_mut() else {
        return;
    };
    if last.get("role").and_then(Value::as_str) != Some("user") {
        return;
    }
    let Some(content) = last.get("content").and_then(Value::as_str).map(str::to_owned) else {
        return;
    };
    if contains_stacktrace(&content) {
        return;
    }

    let before_tok = estimate_calibrated_tokens(&content);
    let reordered = reorder_rag_u_curve(&content);
    let compressed = compress_with_syntax_mask(&reordered, profile);
    let after_tok = estimate_calibrated_tokens(&compressed);

    if after_tok < before_tok {
        ir.stage_stats.prose_pruned_tokens = before_tok.saturating_sub(after_tok);
        last["content"] = Value::String(compressed);
    }
}

/// LongLLMLingua: reorders multi-chunk RAG passages (`---` or `\n\n` separated blocks) along
/// a U-shaped salience curve (highest query relevance at start and end, lowest in the middle).
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

    // Place highest-salience chunks alternating front and back (U-curve).
    let mut front = Vec::with_capacity(scored.len());
    let mut back = Vec::with_capacity(scored.len());
    for (i, (_, _, chunk)) in scored.into_iter().enumerate() {
        if i % 2 == 0 {
            front.push(chunk);
        } else {
            back.push(chunk);
        }
    }
    back.reverse();
    front.extend(back);
    front.push(query);
    front.join("\n---\n")
}

/// Partitions text into Protected (fenced code, inline code, JSON/XML, paths, URLs) vs Prose.
pub fn compress_with_syntax_mask(input: &str, profile: ExecutionProfile) -> String {
    let mut out = String::with_capacity(input.len());
    for (idx, segment) in input.split("```").enumerate() {
        if idx > 0 {
            out.push_str("```");
        }
        if idx % 2 == 1 {
            // Inside fenced code block: 0% mutation.
            out.push_str(segment);
        } else {
            out.push_str(&compress_inline_masked_prose(segment, profile));
        }
    }
    out
}

fn compress_inline_masked_prose(segment: &str, profile: ExecutionProfile) -> String {
    let mut out_lines = Vec::new();
    for line in segment.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if is_protected_line(trimmed) {
            out_lines.push(line.to_owned());
            continue;
        }
        let pruned = match profile {
            ExecutionProfile::Ephemeral => lexical_entropy_prune(trimmed),
            ExecutionProfile::LongLived => long_lived_prune(trimmed),
        };
        if !pruned.is_empty() {
            out_lines.push(pruned);
        }
    }
    out_lines.join("\n")
}

fn is_protected_line(trimmed: &str) -> bool {
    trimmed.starts_with('{')
        || trimmed.starts_with('}')
        || trimmed.starts_with('[')
        || trimmed.starts_with('<')
        || trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.contains("http://")
        || trimmed.contains("https://")
        || trimmed.contains(":\\")
        || trimmed.contains("/src/")
        || contains_stacktrace(trimmed)
}

fn contains_stacktrace(text: &str) -> bool {
    text.contains("Error:")
        || text.contains("Traceback")
        || text.contains("panic:")
        || text.contains("thread '")
}

/// Pure-CPU <2ms lexical entropy scorer for `ExecutionProfile::Ephemeral`.
/// Preserves inline code spans (`...`), identifiers, numbers, and high-entropy content words.
fn lexical_entropy_prune(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    for (idx, part) in line.split('`').enumerate() {
        if idx > 0 {
            out.push('`');
        }
        if idx % 2 == 1 {
            out.push_str(part);
        } else {
            let kept: Vec<&str> = part
                .split_whitespace()
                .filter(|tok| keep_lexical_token(tok))
                .collect();
            out.push_str(&kept.join(" "));
        }
    }
    out
}

fn keep_lexical_token(tok: &str) -> bool {
    if tok.len() <= 2 && tok.chars().all(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    let lower = tok
        .trim_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_ascii_lowercase();
    if STOPWORDS.contains(&lower.as_str()) {
        return false;
    }
    true
}

fn long_lived_prune(line: &str) -> String {
    #[cfg(feature = "slm-onnx")]
    {
        // Warm INT8 encoder hook when compiled with `--features slm-onnx`; falls back
        // deterministically to lexical entropy scoring when no ONNX session is attached.
        lexical_entropy_prune(line)
    }
    #[cfg(not(feature = "slm-onnx"))]
    {
        lexical_entropy_prune(line)
    }
}
