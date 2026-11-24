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

/// Decodes incoming request body when `Content-Encoding: gzip` is present.
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

/// Copies safe headers to upstream while forcing `Accept-Encoding: identity`,
/// recomputing `Content-Length`, and stripping internal `x-aphelion-*` routing headers.
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

/// Resolves target upstream URL honoring:
/// 1. Per-request `X-Aphelion-Upstream: <https://...>` header
/// 2. Multi-provider path prefixes (`/openai/...`, `/anthropic/...`, `/gemini/...`, `/ollama/...`)
/// 3. Automatic dialect path detection (`/messages`, `:generateContent`, `/api/`, default OpenAI).
pub fn resolve_upstream_url(
    targets: &UpstreamTargets,
    headers: &HeaderMap,
    path_and_query: &str,
) -> String {
    if let Some(override_base) = headers
        .get("x-aphelion-upstream")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return join_base_path(override_base, path_and_query);
    }

    if let Some(rest) = path_and_query.strip_prefix("/openai") {
        return join_base_path(&targets.openai, rest);
    }
    if let Some(rest) = path_and_query.strip_prefix("/anthropic") {
        return join_base_path(&targets.anthropic, rest);
    }
    if let Some(rest) = path_and_query.strip_prefix("/gemini") {
        return join_base_path(&targets.gemini, rest);
    }
    if let Some(rest) = path_and_query.strip_prefix("/ollama") {
        return join_base_path(&targets.ollama, rest);
    }

    let base = if path_and_query.contains("/messages") {
        &targets.anthropic
    } else if path_and_query.contains("generateContent") {
        &targets.gemini
    } else if path_and_query.starts_with("/api/") {
        &targets.ollama
    } else {
        &targets.openai
    };
    join_base_path(base, path_and_query)
}

fn join_base_path(base: &str, path: &str) -> String {
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
