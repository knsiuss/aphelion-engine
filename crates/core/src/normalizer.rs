use crate::types::{
    Complexity, Dialect, NormalizedRequest, SessionState, SlmRouteDecision, StageStats,
};
use crate::CoreError;
use serde_json::Value;
use xxhash_rust::xxh3::{xxh3_128, xxh3_64};

/// Parses raw request bytes into a normalized IR and enforces Two-Phase Session Derivation.
pub fn normalize(raw_json: &[u8], state: &mut SessionState) -> Result<NormalizedRequest, CoreError> {
    let root: Value = serde_json::from_slice(raw_json)?;
    let dialect = detect_dialect(&root)?;

    let messages = root
        .get("messages")
        .and_then(Value::as_array)
        .ok_or(CoreError::UnsupportedDialect)?;

    let sacred_boundary = scan_cache_control_boundary(raw_json, &root);
    let is_turn_zero = state.session_id.is_none() || state.turn_index == 0;
    let session_id = derive_two_phase_session(&root, messages, state);
    state.session_id = Some(session_id);

    let active_tool_loop = detect_active_tool_loop(&root, messages);
    let model = root.get("model").and_then(Value::as_str).unwrap_or_default();
    let supports_prefill = model_supports_prefill(model);
    let supports_vision = model_supports_vision(model);

    let pre_compaction_messages = root.get("messages").cloned();
    Ok(NormalizedRequest {
        dialect,
        root,
        sacred_boundary,
        is_turn_zero,
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

fn detect_dialect(root: &Value) -> Result<Dialect, CoreError> {
    if root.get("anthropic_version").is_some()
        || (root.get("max_tokens").is_some() && root.get("system").is_some())
    {
        Ok(Dialect::AnthropicMessages)
    } else if root.get("messages").is_some() {
        Ok(Dialect::OpenAiChat)
    } else {
        Err(CoreError::UnsupportedDialect)
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

/// Phase 1 (Turn-0): XXH3_128(canonical(system) || workspace_fingerprint || first_user[0..512])
/// Phase 2 (Turn-1+): binds `turn_alias = XXH3_64(first_assistant[0..256])` on first assistant turn.
fn derive_two_phase_session(root: &Value, messages: &[Value], state: &mut SessionState) -> u128 {
    if let Some(existing) = state.session_id {
        if state.turn_alias.is_none() {
            if let Some(first_asst) = messages
                .iter()
                .find(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
            {
                let slice = first_asst.to_string();
                let end = slice.len().min(256);
                state.turn_alias = Some(xxh3_64(&slice.as_bytes()[..end]));
            }
        }
        return existing;
    }

    let sys = root.get("system").map(Value::to_string).unwrap_or_default();
    let first_user = messages
        .iter()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        .map(Value::to_string)
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
        for marker in ["cwd:", "CWD:", "workspace:", "repo_root:", "Working directory:"] {
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

fn detect_active_tool_loop(root: &Value, messages: &[Value]) -> bool {
    let has_tools = root
        .get("tools")
        .and_then(Value::as_array)
        .map(|t| !t.is_empty())
        .unwrap_or(false);
    if !has_tools {
        return false;
    }
    if root.get("tool_choice").and_then(Value::as_str) == Some("none") {
        return false;
    }
    let Some(last) = messages.last() else {
        return false;
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
