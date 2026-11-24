use crate::types::Dialect;
use crate::CoreError;
use serde_json::Value;
use std::collections::HashSet;

/// Verifies outbound message-graph legality so Aphelion never causes a proxy-induced HTTP 400.
pub fn validate_outbound_messages(dialect: Dialect, root: &Value) -> Result<(), CoreError> {
    let Some(messages) = root.get("messages").and_then(Value::as_array) else {
        return Err(CoreError::InvalidMessageGraph("missing messages array".to_owned()));
    };
    match dialect {
        Dialect::OpenAiChat => validate_openai(messages),
        Dialect::AnthropicMessages => validate_anthropic(messages),
    }
}

fn validate_openai(messages: &[Value]) -> Result<(), CoreError> {
    let mut expected_tool_ids: HashSet<&str> = HashSet::new();

    for msg in messages {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or_default();
        if role == "tool" {
            let call_id = msg.get("tool_call_id").and_then(Value::as_str);
            match call_id {
                Some(id) if expected_tool_ids.remove(id) => {}
                Some(id) => {
                    return Err(CoreError::InvalidMessageGraph(format!(
                        "orphaned OpenAI tool_call_id: {id}"
                    )));
                }
                None => {
                    // Tool messages without explicit tool_call_id are allowed only when no IDs are pending.
                    if !expected_tool_ids.is_empty() {
                        return Err(CoreError::InvalidMessageGraph(
                            "missing tool_call_id for pending assistant tool_calls".to_owned(),
                        ));
                    }
                }
            }
            continue;
        }

        if !expected_tool_ids.is_empty() {
            return Err(CoreError::InvalidMessageGraph(
                "unmatched OpenAI assistant tool_calls before next non-tool message".to_owned(),
            ));
        }

        if role == "assistant" {
            if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    if let Some(id) = call.get("id").and_then(Value::as_str) {
                        expected_tool_ids.insert(id);
                    }
                }
            }
        }
    }

    if !expected_tool_ids.is_empty() {
        return Err(CoreError::InvalidMessageGraph(
            "dangling OpenAI assistant tool_calls at end of messages".to_owned(),
        ));
    }
    Ok(())
}

fn validate_anthropic(messages: &[Value]) -> Result<(), CoreError> {
    let mut prev_role: Option<&str> = None;
    let mut pending_use_ids: HashSet<&str> = HashSet::new();

    for msg in messages {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or_default();
        if role != "user" && role != "assistant" {
            return Err(CoreError::InvalidMessageGraph(format!(
                "invalid Anthropic role: {role}"
            )));
        }
        if prev_role == Some(role) {
            return Err(CoreError::InvalidMessageGraph(format!(
                "consecutive Anthropic `{role}` messages violate strict role alternation"
            )));
        }
        if is_empty_content(msg.get("content")) {
            return Err(CoreError::InvalidMessageGraph(format!(
                "empty Anthropic message content for role `{role}`"
            )));
        }

        if !pending_use_ids.is_empty() {
            if role != "user" {
                return Err(CoreError::InvalidMessageGraph(
                    "Anthropic tool_use must be immediately followed by user tool_result".to_owned(),
                ));
            }
            let result_ids = extract_anthropic_tool_result_ids(msg);
            for id in &pending_use_ids {
                if !result_ids.contains(id) {
                    return Err(CoreError::InvalidMessageGraph(format!(
                        "missing Anthropic tool_result for tool_use_id `{id}`"
                    )));
                }
            }
            pending_use_ids.clear();
        } else if role == "user" && !extract_anthropic_tool_result_ids(msg).is_empty() {
            return Err(CoreError::InvalidMessageGraph(
                "orphaned Anthropic tool_result without preceding tool_use".to_owned(),
            ));
        }

        if role == "assistant" {
            pending_use_ids = extract_anthropic_tool_use_ids(msg);
        }
        prev_role = Some(role);
    }

    if !pending_use_ids.is_empty() {
        return Err(CoreError::InvalidMessageGraph(
            "dangling Anthropic tool_use at end of messages".to_owned(),
        ));
    }
    Ok(())
}

fn is_empty_content(content: Option<&Value>) -> bool {
    match content {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.trim().is_empty(),
        Some(Value::Array(arr)) => arr.is_empty(),
        _ => false,
    }
}

fn extract_anthropic_tool_use_ids(msg: &Value) -> HashSet<&str> {
    let mut ids = HashSet::new();
    if let Some(blocks) = msg.get("content").and_then(Value::as_array) {
        for b in blocks {
            if b.get("type").and_then(Value::as_str) == Some("tool_use") {
                if let Some(id) = b.get("id").and_then(Value::as_str) {
                    ids.insert(id);
                }
            }
        }
    }
    ids
}

fn extract_anthropic_tool_result_ids(msg: &Value) -> HashSet<&str> {
    let mut ids = HashSet::new();
    if let Some(blocks) = msg.get("content").and_then(Value::as_array) {
        for b in blocks {
            if b.get("type").and_then(Value::as_str) == Some("tool_result") {
                if let Some(id) = b.get("tool_use_id").and_then(Value::as_str) {
                    ids.insert(id);
                }
            }
        }
    }
    ids
}
