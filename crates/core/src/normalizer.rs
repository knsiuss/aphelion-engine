use crate::types::{
    Complexity, Dialect, NormalizedRequest, SessionState, SlmRouteDecision, StageStats,
    WorkloadClass,
};
use crate::workload::{classify_workload, detect_dialect_universal};
use crate::CoreError;
use serde_json::Value;
use xxhash_rust::xxh3::{xxh3_128, xxh3_64};

/// Parses raw request bytes into a normalized IR across all major LLM dialects.
/// Unrecognized JSON schemas or non-JSON bodies degrade safely to `Dialect::Passthrough`
/// (byte-for-byte untouched) with zero errors.
pub fn normalize(
    raw_json: &[u8],
    state: &mut SessionState,
) -> Result<NormalizedRequest, CoreError> {
    let Ok(root) = serde_json::from_slice::<Value>(raw_json) else {
        return Ok(passthrough_ir(raw_json));
    };
    let dialect = detect_dialect_universal(&root);
    if dialect == Dialect::Passthrough {
        return Ok(passthrough_ir(raw_json));
    }

    let sacred_boundary = scan_cache_control_boundary(raw_json, &root);
    let is_turn_zero = state.session_id.is_none() || state.turn_index == 0;
    let session_id = derive_two_phase_session(&root, dialect, state);
    state.session_id = Some(session_id);

    let active_tool_loop = detect_active_tool_loop(&root, dialect);
    let workload = classify_workload(dialect, &root, active_tool_loop);
    let model = root
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    state.active_model = model.clone();
    let supports_prefill = model_supports_prefill(&model);
    let supports_vision = model_supports_vision(&model);
    let pre_compaction_messages = root.get("messages").cloned();

    Ok(NormalizedRequest {
        dialect,
        workload,
        model_id: model,
        root,
        raw_passthrough: None,
        sacred_boundary,
        is_turn_zero,
        ingress_start: 0,
        active_tool_loop,
        complexity: Complexity::Trivial,
        supports_prefill,
        supports_vision,
        tail_directives: Vec::new(),
        assistant_prefill: None,
        slm_route: SlmRouteDecision::Frontier,
        stage_stats: StageStats::default(),
        pre_compaction_messages,
    })
}

fn passthrough_ir(raw: &[u8]) -> NormalizedRequest {
    NormalizedRequest {
        dialect: Dialect::Passthrough,
        workload: WorkloadClass::Passthrough,
        model_id: String::new(),
        root: Value::Null,
        raw_passthrough: Some(raw.to_vec()),
        sacred_boundary: 0,
        is_turn_zero: false,
        ingress_start: 0,
        active_tool_loop: false,
        complexity: Complexity::Trivial,
        supports_prefill: false,
        supports_vision: false,
        tail_directives: Vec::new(),
        assistant_prefill: None,
        slm_route: SlmRouteDecision::Frontier,
        stage_stats: StageStats::default(),
        pre_compaction_messages: None,
    }
}

fn scan_cache_control_boundary(raw_json: &[u8], root: &Value) -> usize {
    let needle = b"cache_control";
    if let Some(pos) = raw_json.windows(needle.len()).rposition(|w| w == needle) {
        let sys_len = root.get("system").map(|v| v.to_string().len()).unwrap_or(0);
        let tools_len = root.get("tools").map(|v| v.to_string().len()).unwrap_or(0);
        pos.saturating_add(needle.len())
            .max(sys_len.saturating_add(tools_len))
    } else {
        0
    }
}

fn derive_two_phase_session(root: &Value, dialect: Dialect, state: &mut SessionState) -> u128 {
    let messages = root.get("messages").and_then(Value::as_array);
    if let Some(existing) = state.session_id {
        if state.turn_alias.is_none() {
            if let Some(msgs) = messages {
                if let Some(first_asst) = msgs
                    .iter()
                    .find(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
                {
                    let slice = first_asst.to_string();
                    state.turn_alias = Some(xxh3_64(&slice.as_bytes()[..slice.len().min(256)]));
                }
            }
        }
        return existing;
    }

    let sys = match dialect {
        Dialect::OpenAiResponses => root.get("instructions"),
        Dialect::GeminiGenerate => root.get("systemInstruction"),
        _ => root.get("system"),
    }
    .map(Value::to_string)
    .unwrap_or_default();

    let first_user = messages
        .and_then(|msgs| {
            msgs.iter()
                .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
                .map(Value::to_string)
        })
        .or_else(|| root.get("input").map(Value::to_string))
        .or_else(|| root.get("contents").map(Value::to_string))
        .or_else(|| root.get("prompt").map(Value::to_string))
        .unwrap_or_default();

    let workspace_fp = extract_workspace_fingerprint(&sys, &first_user);
    let end = first_user.len().min(512);
    let mut seed = Vec::with_capacity(sys.len() + workspace_fp.len() + end + 2);
    seed.extend_from_slice(sys.as_bytes());
    seed.push(b'|');
    seed.extend_from_slice(workspace_fp.as_bytes());
    seed.push(b'|');
    seed.extend_from_slice(&first_user.as_bytes()[..end]);
    xxh3_128(&seed)
}

pub fn extract_workspace_fingerprint(system: &str, first_user: &str) -> String {
    for source in [system, first_user] {
        for marker in [
            "cwd:",
            "CWD:",
            "workspace:",
            "repo_root:",
            "Working directory:",
        ] {
            if let Some(idx) = source.find(marker) {
                let rest = &source[idx + marker.len()..];
                let line = rest
                    .split(['\n', '\r', '"', '}', ';'])
                    .next()
                    .unwrap_or_default()
                    .trim();
                if !line.is_empty() {
                    return line.to_owned();
                }
            }
        }
    }
    String::new()
}

fn detect_active_tool_loop(root: &Value, dialect: Dialect) -> bool {
    let has_tools = root
        .get("tools")
        .and_then(Value::as_array)
        .map(|t| !t.is_empty())
        .unwrap_or(false);
    if !has_tools || root.get("tool_choice").and_then(Value::as_str) == Some("none") {
        return false;
    }
    if dialect == Dialect::OpenAiResponses {
        return true;
    }
    let Some(last) = root
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|m| m.last())
    else {
        return has_tools;
    };
    let role = last.get("role").and_then(Value::as_str).unwrap_or_default();
    if role == "tool" {
        return true;
    }
    if let Some(blocks) = last.get("content").and_then(Value::as_array) {
        if blocks
            .iter()
            .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
        {
            return true;
        }
    }
    role == "user" && has_tools
}

fn model_supports_prefill(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    !(m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") || m.starts_with("gpt-5"))
}

fn model_supports_vision(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m.contains("claude-3")
        || m.contains("claude-sonnet-4")
        || m.contains("claude-opus-4")
        || m.contains("gpt-4o")
        || m.contains("gpt-4.1")
        || m.contains("gemini")
}
