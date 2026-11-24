use crate::types::ExecutionProfile;
use std::collections::HashSet;

const STOPWORDS: &[&str] = &[
    "the",
    "a",
    "an",
    "of",
    "to",
    "in",
    "for",
    "on",
    "with",
    "at",
    "by",
    "from",
    "as",
    "is",
    "was",
    "are",
    "were",
    "be",
    "been",
    "being",
    "that",
    "this",
    "these",
    "those",
    "it",
    "its",
    "very",
    "just",
    "really",
    "actually",
    "basically",
    "simply",
    "please",
    "kindly",
    "note",
];

pub fn reorder_rag_u_curve(input: &str) -> String {
    let chunks: Vec<&str> = input.split("\n---\n").collect();
    if chunks.len() < 4 {
        return input.to_owned();
    }
    let query = chunks.last().copied().unwrap_or_default();
    let query_terms: HashSet<String> = query
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_ascii_lowercase()
        })
        .filter(|w| w.len() > 2)
        .collect();
    let mut scored: Vec<(usize, usize, &str)> = chunks[..chunks.len() - 1]
        .iter()
        .enumerate()
        .map(|(idx, chunk)| {
            let score = chunk
                .split_whitespace()
                .filter(|w| {
                    let c = w
                        .trim_matches(|ch: char| !ch.is_alphanumeric())
                        .to_ascii_lowercase();
                    query_terms.contains(&c)
                })
                .count();
            (score, idx, *chunk)
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let (mut front, mut back) = (
        Vec::with_capacity(scored.len()),
        Vec::with_capacity(scored.len()),
    );
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

pub fn compress_with_syntax_mask(input: &str, profile: ExecutionProfile) -> String {
    let mut out = String::with_capacity(input.len());
    for (idx, segment) in input.split("```").enumerate() {
        if idx > 0 {
            out.push_str("```");
        }
        if idx % 2 == 1 {
            out.push_str(segment);
        } else {
            out.push_str(&compress_inline_masked_prose(segment, profile));
        }
    }
    out
}

fn compress_inline_masked_prose(segment: &str, _profile: ExecutionProfile) -> String {
    segment
        .lines()
        .filter_map(|line| {
            let t = line.trim();
            if t.is_empty() {
                None
            } else if is_protected_line(t) {
                Some(line.to_owned())
            } else {
                let p = lexical_entropy_prune(t);
                (!p.is_empty()).then_some(p)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_protected_line(t: &str) -> bool {
    t.starts_with('{')
        || t.starts_with('}')
        || t.starts_with('[')
        || t.starts_with('<')
        || t.starts_with("//")
        || t.starts_with('#')
        || t.contains("http://")
        || t.contains("https://")
        || t.contains(":\\")
        || t.contains("/src/")
        || contains_stacktrace(t)
}

pub fn contains_stacktrace(text: &str) -> bool {
    text.contains("Error:")
        || text.contains("Traceback")
        || text.contains("panic:")
        || text.contains("thread '")
}

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
                .filter(|t| keep_lexical_token(t))
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
    !STOPWORDS.contains(&lower.as_str())
}
