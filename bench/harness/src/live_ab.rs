use aphelion_core::patcher::validate_syntax;
use aphelion_core::sse_dialect::extract_usage_from_json;
use aphelion_core::types::{CodeLang, ExecutionProfile, SessionState};
use aphelion_telemetry::ledger::count_self_doubt_hedges;
use reqwest::Client;
use serde_json::Value;
use std::env;
use std::time::Instant;

#[derive(Debug, Default)]
pub struct ArmMetrics {
    pub cached_tokens: usize,
    pub total_ms: Vec<u64>,
    pub reasoning_tokens: usize,
    pub action_tokens: usize,
    pub hedges: usize,
    pub ast_errors: usize,
    pub invalid_tool_json: usize,
}

impl ArmMetrics {
    pub fn p50_ms(&self) -> u64 {
        percentile(&self.total_ms, 50)
    }

    pub fn p95_ms(&self) -> u64 {
        percentile(&self.total_ms, 95)
    }

    pub fn rho_ra(&self) -> f64 {
        if self.action_tokens == 0 {
            self.reasoning_tokens as f64
        } else {
            self.reasoning_tokens as f64 / self.action_tokens as f64
        }
    }

    pub fn hedge_density(&self) -> f64 {
        if self.reasoning_tokens == 0 {
            0.0
        } else {
            (self.hedges as f64 * 100.0) / (self.reasoning_tokens as f64)
        }
    }
}

/// Live Shadow A/B replay comparing Control (Raw) vs. Treatment (Aphelion) against
/// any live OpenAI, Anthropic, 9Router, or vLLM endpoint.
pub async fn run_live_ab(
    upstream: &str,
    api_key_env: &str,
    model_override: Option<&str>,
    trajectory: &[Vec<u8>],
) {
    let api_key = env::var(api_key_env).unwrap_or_default();
    let client = Client::new();
    let mut control = ArmMetrics::default();
    let mut treatment = ArmMetrics::default();
    let mut state = SessionState::default();

    for raw in trajectory {
        let base_body = maybe_override_model(raw, model_override);
        execute_arm(&client, upstream, &api_key, &base_body, &mut control).await;

        let opt_body = match aphelion_core::prepare_request(
            &base_body,
            ExecutionProfile::Ephemeral,
            &mut state,
        ) {
            Ok(out) => out.body,
            Err(_) => base_body,
        };
        execute_arm(&client, upstream, &api_key, &opt_body, &mut treatment).await;
    }

    println!("\n### Live Shadow A/B Report ({upstream})");
    println!("| Metric | Control (Raw) | Treatment (Aphelion) |");
    println!("| :--- | ---: | ---: |");
    println!("| Verified Cached Tokens | {} | {} |", control.cached_tokens, treatment.cached_tokens);
    println!("| Latency p50 / p95 (ms) | {} / {} | {} / {} |", control.p50_ms(), control.p95_ms(), treatment.p50_ms(), treatment.p95_ms());
    println!("| Reasoning-to-Action (rho_RA) | {:.2} | {:.2} |", control.rho_ra(), treatment.rho_ra());
    println!("| Hedge Density (per 100 tok) | {:.2} | {:.2} |", control.hedge_density(), treatment.hedge_density());
    println!("| AST Syntax Errors | {} | {} |", control.ast_errors, treatment.ast_errors);
    println!("| Invalid Tool-Call JSON | {} | {} |", control.invalid_tool_json, treatment.invalid_tool_json);
}

async fn execute_arm(
    client: &Client,
    upstream: &str,
    api_key: &str,
    body: &[u8],
    metrics: &mut ArmMetrics,
) {
    let start = Instant::now();
    let mut req = client
        .post(upstream)
        .header("content-type", "application/json")
        .header("accept-encoding", "identity")
        .body(body.to_vec());
    if !api_key.is_empty() {
        req = req
            .header("authorization", format!("Bearer {api_key}"))
            .header("x-api-key", api_key);
    }
    let Ok(resp) = req.send().await else {
        return;
    };
    let elapsed = start.elapsed().as_millis() as u64;
    metrics.total_ms.push(elapsed);

    let Ok(bytes) = resp.bytes().await else {
        return;
    };
    let Ok(json) = serde_json::from_slice::<Value>(&bytes) else {
        return;
    };
    if let Some(u) = extract_usage_from_json(&json) {
        metrics.cached_tokens += u.cached_tokens;
        metrics.reasoning_tokens += u.reasoning_tokens;
        metrics.action_tokens += u.output_tokens.saturating_sub(u.reasoning_tokens);
    }
    if let Some(text) = json.pointer("/choices/0/message/content").and_then(Value::as_str) {
        metrics.hedges += count_self_doubt_hedges(text);
        if text.contains("```rust\n") && !validate_syntax(CodeLang::Rust, extract_rust_block(text)) {
            metrics.ast_errors += 1;
        }
    }
    if let Some(calls) = json.pointer("/choices/0/message/tool_calls").and_then(Value::as_array) {
        for c in calls {
            if let Some(args) = c.pointer("/function/arguments").and_then(Value::as_str) {
                if serde_json::from_str::<Value>(args).is_err() {
                    metrics.invalid_tool_json += 1;
                }
            }
        }
    }
}

fn maybe_override_model(raw: &[u8], model_override: Option<&str>) -> Vec<u8> {
    let Some(m) = model_override else {
        return raw.to_vec();
    };
    let Ok(mut val) = serde_json::from_slice::<Value>(raw) else {
        return raw.to_vec();
    };
    val["model"] = Value::String(m.to_owned());
    serde_json::to_vec(&val).unwrap_or_else(|_| raw.to_vec())
}

fn extract_rust_block(text: &str) -> &str {
    let Some(start) = text.find("```rust\n") else {
        return text;
    };
    let code = &text[start + 8..];
    code.split("```").next().unwrap_or(code)
}

fn percentile(vals: &[u64], pct: usize) -> u64 {
    if vals.is_empty() {
        return 0;
    }
    let mut sorted = vals.to_vec();
    sorted.sort_unstable();
    let idx = ((sorted.len() - 1) * pct) / 100;
    sorted[idx]
}
