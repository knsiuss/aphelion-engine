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

pub fn decode_incoming_body(headers: &HeaderMap, raw: &[u8]) -> Vec<u8> {
    let is_gzip = headers
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_ascii_lowercase().contains("gzip"))
        .unwrap_or(false);
    if !is_gzip {
        return raw.to_vec();
    }
    let mut decoder = GzDecoder::new(raw);
    let mut decoded = Vec::with_capacity(raw.len().saturating_mul(2));
    if decoder.read_to_end(&mut decoded).is_ok() {
        decoded
    } else {
        raw.to_vec()
    }
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

    RequestOptions {
        tenant_hash,
        provider_id: provider_id.to_owned(),
        output_verbose,
        reasoning_effort,
        compact_mode,
    }
}

pub fn apply_proxy_headers(
    mut builder: RequestBuilder,
    incoming: &HeaderMap,
    body_len: usize,
) -> RequestBuilder {
    for (name, value) in incoming {
        let lower = name.as_str().to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "host"
                | "content-length"
                | "content-encoding"
                | "accept-encoding"
                | "transfer-encoding"
                | "x-aphelion-upstream"
                | "x-aphelion-bypass"
                | "x-aphelion-output-mode"
                | "x-aphelion-reasoning-effort"
                | "x-aphelion-compact"
        ) {
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

pub fn join_base_path(base: &str, path: &str) -> String {
    let clean = base.trim_end_matches('/');
    if clean.ends_with("/v1") && path.starts_with("/v1") {
        format!("{}{}", clean.trim_end_matches("/v1"), path)
    } else {
        format!("{clean}{path}")
    }
}

pub fn extract_model(raw: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(raw)
        .ok()
        .and_then(|v| v.get("model").and_then(|m| m.as_str()).map(str::to_owned))
        .unwrap_or_else(|| "gpt-4o".to_owned())
}
