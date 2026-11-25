use aphelion_core::types::{CompactMode, RequestOptions};
use hyper::HeaderMap;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimePolicy {
    pub output_mode: String,
    pub compaction_mode: String,
    pub reasoning_effort: String,
}

impl Default for RuntimePolicy {
    fn default() -> Self {
        Self {
            output_mode: "concise".to_owned(),
            compaction_mode: "auto".to_owned(),
            reasoning_effort: "default".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PolicyPatch {
    pub output_mode: Option<String>,
    pub compaction_mode: Option<String>,
    pub reasoning_effort: Option<String>,
}

impl RuntimePolicy {
    pub fn apply_patch(&mut self, patch: PolicyPatch) {
        if let Some(m) = patch.output_mode {
            if matches!(m.as_str(), "concise" | "verbose") {
                self.output_mode = m;
            }
        }
        if let Some(c) = patch.compaction_mode {
            if matches!(c.as_str(), "auto" | "off") {
                self.compaction_mode = c;
            }
        }
        if let Some(r) = patch.reasoning_effort {
            if matches!(r.as_str(), "default" | "low" | "medium" | "high") {
                self.reasoning_effort = r;
            }
        }
    }

    /// Merges gateway-wide runtime policy into per-request `RequestOptions`.
    pub fn apply_to_request(&self, opts: &mut RequestOptions, headers: &HeaderMap) {
        if !headers.contains_key("x-aphelion-output-mode") && self.output_mode == "verbose" {
            opts.output_verbose = true;
        }
        if !headers.contains_key("x-aphelion-compact") && self.compaction_mode == "off" {
            opts.compact_mode = CompactMode::Off;
        }
        if opts.reasoning_effort.is_none() && self.reasoning_effort != "default" {
            opts.reasoning_effort = Some(self.reasoning_effort.clone());
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryEvent {
    pub timestamp_ms: i64,
    pub client: String,
    pub provider: String,
    pub model: String,
    pub status_code: u16,
    pub ttft_ms: u64,
    pub cache_hit_pct: f64,
    pub cached_in: usize,
    pub uncached_in: usize,
    pub output: usize,
    pub reasoning: usize,
    pub net_saved_usd: f64,
    pub badges: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpstreamHealthNode {
    pub label: String,
    pub health: String,
    pub avg_ttft_ms: u64,
    pub req_count: usize,
    pub error_count: usize,
    pub last_seen_ms: i64,
}

#[derive(Debug, Default)]
pub struct TopologyTracker {
    pub clients: BTreeMap<String, usize>,
    pub upstreams: BTreeMap<String, UpstreamHealthNode>,
    pub recent: VecDeque<TelemetryEvent>,
}

impl TopologyTracker {
    pub fn record(&mut self, ev: TelemetryEvent) {
        *self.clients.entry(ev.client.clone()).or_insert(0) += 1;
        let node = self
            .upstreams
            .entry(ev.provider.clone())
            .or_insert_with(|| UpstreamHealthNode {
                label: ev.provider.clone(),
                health: "healthy".to_owned(),
                avg_ttft_ms: ev.ttft_ms,
                req_count: 0,
                error_count: 0,
                last_seen_ms: ev.timestamp_ms,
            });
        let total_ttft = node.avg_ttft_ms.saturating_mul(node.req_count as u64) + ev.ttft_ms;
        node.req_count += 1;
        node.avg_ttft_ms = total_ttft / (node.req_count as u64).max(1);
        node.last_seen_ms = ev.timestamp_ms;
        if ev.status_code == 429 {
            node.error_count += 1;
            node.health = "cooldown_429".to_owned();
        } else if ev.status_code >= 500 {
            node.error_count += 1;
            node.health = "degraded".to_owned();
        } else if (200..=299).contains(&ev.status_code) {
            node.health = "healthy".to_owned();
        }
        if self.recent.len() >= 50 {
            self.recent.pop_back();
        }
        self.recent.push_front(ev);
    }
}

/// Detects a non-sensitive client tag from `X-Title`, `HTTP-Referer`, or `User-Agent`.
pub fn detect_client_tag(headers: &HeaderMap) -> String {
    let probe = ["x-title", "http-referer", "user-agent"]
        .into_iter()
        .filter_map(|k| headers.get(k).and_then(|v| v.to_str().ok()))
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    if probe.contains("opencode") {
        "OpenCode".to_owned()
    } else if probe.contains("claude") {
        "Claude Code".to_owned()
    } else if probe.contains("cursor") {
        "Cursor".to_owned()
    } else if probe.contains("cline") || probe.contains("roo") {
        "Cline".to_owned()
    } else if probe.contains("aider") {
        "Aider".to_owned()
    } else if probe.contains("python") || probe.contains("openai") {
        "SDK".to_owned()
    } else {
        "CLI".to_owned()
    }
}
