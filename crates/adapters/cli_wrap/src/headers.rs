use flate2::read::GzDecoder;
use hyper::HeaderMap;
use reqwest::RequestBuilder;
use std::io::Read;

/// Decodes incoming request body when `Content-Encoding: gzip` is present.
/// (`Transfer-Encoding: chunked` is already decoded transparently by hyper.)
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

/// Copies safe headers to upstream while:
/// - stripping `Host`, `Content-Length`, `Content-Encoding`, `Transfer-Encoding`, `Accept-Encoding`
/// - forcing `Accept-Encoding: identity` so SSE responses are never compressed mid-stream
/// - recomputing exact `Content-Length` for modified JSON bodies
/// - forwarding `Authorization`, `x-api-key`, `anthropic-version`, etc. untouched.
pub fn apply_proxy_headers(
    mut builder: RequestBuilder,
    incoming: &HeaderMap,
    body_len: usize,
) -> RequestBuilder {
    for (name, value) in incoming {
        let lower = name.as_str().to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "host" | "content-length" | "content-encoding" | "accept-encoding" | "transfer-encoding"
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

/// Resolves target upstream URL supporting seamless chaining to local routers (e.g. 9Router).
pub fn resolve_upstream_url(
    upstream_openai: &str,
    upstream_anthropic: &str,
    path_and_query: &str,
) -> String {
    let base = if path_and_query.contains("/messages") {
        upstream_anthropic.trim_end_matches('/')
    } else {
        upstream_openai.trim_end_matches('/')
    };
    if base.ends_with("/v1") && path_and_query.starts_with("/v1") {
        format!("{}{}", base.trim_end_matches("/v1"), path_and_query)
    } else {
        format!("{base}{path_and_query}")
    }
}

pub fn extract_model(raw: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(raw)
        .ok()
        .and_then(|v| v.get("model").and_then(|m| m.as_str()).map(str::to_owned))
        .unwrap_or_else(|| "gpt-4o".to_owned())
}
