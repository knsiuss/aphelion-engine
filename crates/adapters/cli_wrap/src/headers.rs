use aphelion_core::types::{CompactMode, RequestOptions};
use flate2::read::GzDecoder;
use hyper::HeaderMap;
use reqwest::RequestBuilder;
use std::io::Read;

pub struct UpstreamTargets {
    pub openai: String,
    pub anthropic: String,
    pub gemini: String,
    pub ollama: String,
}

/// Decodes incoming request body with a strict `max_bytes` decompression cap
/// to prevent gzip decompression bombs (`None` when decompressed size > `max_bytes`).
pub fn decode_incoming_body_bounded(
    headers: &HeaderMap,
    raw: &[u8],
    max_bytes: usize,
) -> Option<Vec<u8>> {
    if raw.len() > max_bytes {
        return None;
    }
    let is_gzip = headers
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_ascii_lowercase().contains("gzip"))
        .unwrap_or(false);
    if !is_gzip {
        return Some(raw.to_vec());
    }
    let decoder = GzDecoder::new(raw);
    let limit = (max_bytes as u64).saturating_add(1);
    let mut bounded = decoder.take(limit);
    let mut decoded = Vec::with_capacity(raw.len().saturating_mul(2).min(max_bytes));
    if bounded.read_to_end(&mut decoded).is_ok() {
        if decoded.len() > max_bytes {
            None
        } else {
            Some(decoded)
        }
    } else {
        Some(raw.to_vec())
    }
}

pub fn decode_incoming_body(headers: &HeaderMap, raw: &[u8]) -> Vec<u8> {
    decode_incoming_body_bounded(headers, raw, 64 * 1024 * 1024).unwrap_or_else(|| raw.to_vec())
}

pub fn has_bypass_header(headers: &HeaderMap) -> bool {
    headers
        .get("x-aphelion-bypass")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(false)
}

/// Parses per-request Aphelion control headers into `RequestOptions`.
pub fn parse_request_options(
    headers: &HeaderMap,
    tenant_hash: u64,
    provider_id: &str,
) -> RequestOptions {
    let output_verbose = headers
        .get("x-aphelion-output-mode")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("verbose"))
        .unwrap_or(false);
    let reasoning_effort = headers
        .get("x-aphelion-reasoning-effort")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    let compact_mode = headers
        .get("x-aphelion-compact")
        .and_then(|v| v.to_str().ok())
        .map(|s| match s.trim().to_ascii_lowercase().as_str() {
            "force" => CompactMode::Force,
            "off" => CompactMode::Off,
            _ => CompactMode::Auto,
        })
        .unwrap_or(CompactMode::Auto);

    let (comp_thresh, tail_keep, slm_ep) = crate::config::GatewayConfig::env_core_overrides();
    RequestOptions {
        tenant_hash,
        provider_id: provider_id.to_owned(),
        output_verbose,
        reasoning_effort,
        compact_mode,
        compaction_threshold_tokens: Some(comp_thresh),
        locked_tail_messages: Some(tail_keep),
        slm_endpoint: Some(slm_ep),
        routing: Default::default(),
    }
}

pub fn apply_proxy_headers(
    mut builder: RequestBuilder,
    incoming: &HeaderMap,
    body_len: usize,
) -> RequestBuilder {
    for (name, value) in incoming {
        let lower = name.as_str().to_ascii_lowercase();
        if lower.starts_with("x-aphelion-")
            || matches!(
                lower.as_str(),
                "host"
                    | "content-length"
                    | "content-encoding"
                    | "accept-encoding"
                    | "transfer-encoding"
                    | "connection"
                    | "keep-alive"
                    | "te"
                    | "trailer"
                    | "upgrade"
                    | "proxy-authorization"
            )
        {
            continue;
        }
        if let Ok(v) = value.to_str() {
            builder = builder.header(name.as_str(), v);
        }
    }
    builder
        .header("accept-encoding", "identity")
        .header("content-length", body_len.to_string())
}

/// Safely joins an upstream base URL and relative request path, ensuring `@` or `.`
/// in `path` can never alter the authority/host of `base` (prevents `/ollama@169.254...` SSRF).
pub fn join_base_path(base: &str, path: &str) -> String {
    let clean = base.trim_end_matches('/');
    let rel = path.trim_start_matches('/').trim_start_matches('@');
    if rel.is_empty() {
        return clean.to_owned();
    }
    if clean.ends_with("/v1") && (rel == "v1" || rel.starts_with("v1/")) {
        let after_v1 = rel.strip_prefix("v1").unwrap_or(rel).trim_start_matches('/');
        if after_v1.is_empty() {
            clean.to_owned()
        } else {
            format!("{clean}/{after_v1}")
        }
    } else {
        format!("{clean}/{rel}")
    }
}

pub fn extract_model(raw: &[u8]) -> String {
    extract_model_with_path(raw, "")
}

pub fn extract_model_with_path(raw: &[u8], path: &str) -> String {
    if let Some(m) = serde_json::from_slice::<serde_json::Value>(raw)
        .ok()
        .and_then(|v| v.get("model").and_then(|m| m.as_str()).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned))
    {
        return m;
    }
    if let Some(idx) = path.find("/models/") {
        let rest = &path[idx + 8..];
        let model = rest.split([':', '/', '?']).next().unwrap_or_default().trim();
        if !model.is_empty() {
            return model.to_owned();
        }
    }
    "gpt-4o".to_owned()
}
