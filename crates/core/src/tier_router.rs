use crate::defaults::DEFAULT_FAST_MAX_CONTEXT_TOKENS;
use crate::diet_gc::estimate_calibrated_tokens;
use crate::ingress_trim::contains_any_failure_marker;
use crate::types::{Complexity, NormalizedRequest, RequestOptions, WorkloadClass};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Runtime routing & opt-in auto-reasoning knobs (zero hardcoded model IDs).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TierRoutingConfig {
    /// `"off"` | `"safe"` | `"aggressive"`
    pub route_mode: String,
    pub fast_model: Option<String>,
    pub fast_max_context_tokens: Option<usize>,
    /// Opt-in (`false` by default) to preserve 100% model IQ unless user enables `"safe"`.
    pub auto_reasoning_safe: bool,
}

/// Cache-Aware 9Router-Style Tier Gate:
/// Routes stateless / short-context trivial requests (`<= fast_max_context_tokens`) to
/// `fast_model` while keeping warm cached multi-turn coding sessions (`> 12k` or Deep/Error)
/// 100% sticky on the primary frontier model so KV-cache hit rate is never degraded.
pub fn apply_tier_routing(
    ir: &mut NormalizedRequest,
    opts: &RequestOptions,
    prior_turns: usize,
) -> Option<String> {
    let fast_model = opts
        .routing
        .fast_model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let mode = opts.routing.route_mode.trim().to_ascii_lowercase();
    if mode.is_empty() || mode == "off" {
        return None;
    }
    if ir.model_id.eq_ignore_ascii_case(fast_model) || ir.complexity == Complexity::Deep {
        return None;
    }

    let max_ctx = opts
        .routing
        .fast_max_context_tokens
        .unwrap_or(DEFAULT_FAST_MAX_CONTEXT_TOKENS);
    let est_tokens = estimate_messages_tokens(&ir.root);
    if est_tokens > max_ctx {
        return None;
    }

    let msg_count = ir
        .root
        .get("messages")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let last_text = extract_tail_text(&ir.root);
    if contains_any_failure_marker(&last_text) {
        return None;
    }

    let eligible = match mode.as_str() {
        "aggressive" => true,
        "safe" => {
            // Sticky session protection: never break warm multi-turn sessions (`prior_turns >= 2`).
            if prior_turns >= 2 && msg_count > 3 {
                return None;
            }
            let has_code_fence = last_text.contains("```");
            matches!(ir.workload, WorkloadClass::SingleTurnRagOrChat)
                || (msg_count <= 2 && !has_code_fence && est_tokens <= max_ctx)
        }
        _ => false,
    };

    if !eligible {
        return None;
    }

    ir.root["model"] = Value::String(fast_model.to_owned());
    ir.model_id = fast_model.to_owned();
    ir.stage_stats.routed_fast_model = Some(fast_model.to_owned());
    Some(fast_model.to_owned())
}

/// Returns true if the latest message is a deterministic mechanical step safe for `low`
/// reasoning when the user has explicitly opted into `APHELION_AUTO_REASONING=safe`.
pub fn is_safe_low_reasoning_turn(ir: &NormalizedRequest) -> bool {
    if ir.is_turn_zero || ir.complexity == Complexity::Deep {
        return false;
    }
    let last = extract_tail_text(&ir.root);
    if last.is_empty() || contains_any_failure_marker(&last) {
        return false;
    }
    let lower = last.to_ascii_lowercase();
    let is_short_confirm = last.len() <= 200
        && (lower.contains("created successfully")
            || lower.contains("updated successfully")
            || lower.contains("edited")
            || lower.contains("has been updated")
            || lower.contains("wrote "));
    let is_passing_test = lower.contains("[aphelion test trim:")
        || lower.contains("0 failed")
        || lower.contains("test result: ok");
    is_short_confirm || is_passing_test
}

/// Returns true if a `flash_first` combo session should latch onto the Frontier model
/// (deep reasoning, failure/traceback in tail, or circuit breaker tripped).
pub fn should_escalate_to_frontier(ir: &NormalizedRequest) -> bool {
    if ir.complexity == Complexity::Deep || ir.stage_stats.circuit_breaker_tripped {
        return true;
    }
    contains_any_failure_marker(&extract_tail_text(&ir.root))
}

fn estimate_messages_tokens(root: &Value) -> usize {
    if let Some(msgs) = root.get("messages").or_else(|| root.get("input")) {
        estimate_calibrated_tokens(&msgs.to_string())
    } else {
        estimate_calibrated_tokens(&root.to_string())
    }
}

fn extract_tail_text(root: &Value) -> String {
    let Some(last) = root
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|m| m.last())
    else {
        return String::new();
    };
    if let Some(s) = last.get("content").and_then(Value::as_str) {
        return s.to_owned();
    }
    let Some(arr) = last.get("content").and_then(Value::as_array) else {
        return String::new();
    };
    arr.iter()
        .filter_map(|b| {
            b.get("text")
                .or_else(|| b.get("content"))
                .and_then(Value::as_str)
        })
        .collect::<Vec<_>>()
        .join("\n")
}
