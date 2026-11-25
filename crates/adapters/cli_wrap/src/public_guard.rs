pub use crate::metrics::MetricsRegistry;
use hyper::{HeaderMap, StatusCode};
use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use xxhash_rust::xxh3::xxh3_64;

#[derive(Debug, Clone)]
pub struct PublicConfig {
    pub public_mode: bool,
    pub rpm_limit: u32,
    pub max_body_bytes: usize,
    pub admin_token: Option<String>,
}

impl Default for PublicConfig {
    fn default() -> Self {
        Self {
            public_mode: false,
            rpm_limit: 120,
            max_body_bytes: 10 * 1024 * 1024,
            admin_token: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GuardRejection {
    pub status: StatusCode,
    pub message: String,
    pub retry_after_secs: Option<u64>,
}

/// Extracts cryptographic `tenant_hash = XXH3_64(auth_token)` in memory only (never logged).
/// In `--public` mode, missing `Authorization` / `x-api-key` returns `401 Unauthorized`.
pub fn extract_tenant_hash(headers: &HeaderMap, public_mode: bool) -> Result<u64, GuardRejection> {
    let raw = extract_raw_credential(headers);
    if !public_mode {
        return Ok(raw.map(|s| xxh3_64(s.as_bytes())).unwrap_or(0));
    }
    let Some(token) = raw.filter(|s| !s.is_empty()) else {
        return Err(GuardRejection {
            status: StatusCode::UNAUTHORIZED,
            message: "Authorization or x-api-key header required in --public mode".to_owned(),
            retry_after_secs: None,
        });
    };
    Ok(xxh3_64(token.as_bytes()))
}

pub fn extract_raw_credential(headers: &HeaderMap) -> Option<&str> {
    if let Some(auth) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        let stripped = auth
            .strip_prefix("Bearer ")
            .or_else(|| auth.strip_prefix("bearer "))
            .unwrap_or(auth)
            .trim();
        if !stripped.is_empty() {
            return Some(stripped);
        }
    }
    headers
        .get("x-api-key")
        .or_else(|| headers.get("x-goog-api-key"))
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Per-tenant 60-second sliding-window rate limiter returning `429 Too Many Requests`.
#[derive(Default)]
pub struct SlidingWindowLimiter {
    windows: Mutex<HashMap<u64, VecDeque<Instant>>>,
}

impl SlidingWindowLimiter {
    pub fn check(&self, tenant_hash: u64, limit: u32) -> Result<(), GuardRejection> {
        if limit == 0 {
            return Ok(());
        }
        let now = Instant::now();
        let cutoff = now.checked_sub(Duration::from_secs(60)).unwrap_or(now);
        let Ok(mut map) = self.windows.lock() else {
            return Ok(());
        };
        let queue = map.entry(tenant_hash).or_default();
        while queue.front().is_some_and(|&t| t < cutoff) {
            queue.pop_front();
        }
        if queue.len() >= limit as usize {
            let retry = queue
                .front()
                .map(|&t| {
                    60u64
                        .saturating_sub(now.saturating_duration_since(t).as_secs())
                        .max(1)
                })
                .unwrap_or(60);
            return Err(GuardRejection {
                status: StatusCode::TOO_MANY_REQUESTS,
                message: "Rate limit exceeded".to_owned(),
                retry_after_secs: Some(retry),
            });
        }
        queue.push_back(now);
        Ok(())
    }
}

/// SSRF Guard for `--public` mode: blocks loopback, private, link-local, and cloud metadata IPs.
pub fn validate_upstream_ssrf(url_str: &str) -> Result<(), GuardRejection> {
    let parsed = reqwest::Url::parse(url_str).map_err(|_| GuardRejection {
        status: StatusCode::FORBIDDEN,
        message: "Invalid upstream URL".to_owned(),
        retry_after_secs: None,
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(ssrf_reject("Only http/https schemes permitted"));
    }
    let Some(host) = parsed.host_str() else {
        return Err(ssrf_reject("Missing upstream host"));
    };
    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".local") || lower.ends_with(".internal") {
        return Err(ssrf_reject("Loopback/internal host blocked by SSRF guard"));
    }
    let trimmed = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = trimmed.parse::<IpAddr>() {
        if is_forbidden_ip(ip) {
            return Err(ssrf_reject(
                "Private/loopback/link-local IP blocked by SSRF guard",
            ));
        }
        return Ok(());
    }
    let port = parsed.port_or_known_default().unwrap_or(443);
    if let Ok(addrs) = (trimmed, port).to_socket_addrs() {
        for addr in addrs {
            if is_forbidden_ip(addr.ip()) {
                return Err(ssrf_reject(
                    "Resolved private/loopback IP blocked by SSRF guard",
                ));
            }
        }
    }
    Ok(())
}

fn ssrf_reject(msg: &str) -> GuardRejection {
    GuardRejection {
        status: StatusCode::FORBIDDEN,
        message: msg.to_owned(),
        retry_after_secs: None,
    }
}

fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xC0) == 64)
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

pub fn render_health_json(public_mode: bool) -> String {
    let mode = if public_mode { "public" } else { "local" };
    format!(r#"{{"status":"ok","version":"3.0","mode":"{mode}"}}"#)
}
