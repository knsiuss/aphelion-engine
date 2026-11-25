use crate::types::{Dialect, WorkloadClass};
use serde_json::Value;

/// Detects API dialect across OpenAI Chat, OpenAI Responses, Anthropic Messages,
/// Google Gemini, Ollama Native, or transparent `Passthrough` for unknown schemas.
pub fn detect_dialect_universal(root: &Value) -> Dialect {
    let Some(obj) = root.as_object() else {
        return Dialect::Passthrough;
    };
    if obj.contains_key("anthropic_version")
        || (obj.contains_key("max_tokens") && obj.contains_key("system"))
    {
        return Dialect::AnthropicMessages;
    }
    if obj.get("contents").and_then(Value::as_array).is_some() {
        return Dialect::GeminiGenerate;
    }
    if obj.contains_key("input") && !obj.contains_key("messages") {
        // Distinguish OpenAI Responses API from /v1/embeddings (which only has input + model/encoding_format).
        if obj.contains_key("instructions")
            || obj.contains_key("previous_response_id")
            || obj.contains_key("tools")
            || obj.contains_key("text")
            || obj.contains_key("max_output_tokens")
            || obj.get("input").and_then(Value::as_array).is_some()
        {
            return Dialect::OpenAiResponses;
        }
        return Dialect::Passthrough;
    }
    if obj.get("messages").and_then(Value::as_array).is_some() {
        if obj.contains_key("options") || obj.contains_key("keep_alive") {
            return Dialect::OllamaNative;
        }
        return Dialect::OpenAiChat;
    }
    if obj.get("prompt").and_then(Value::as_str).is_some() && obj.contains_key("model") {
        return Dialect::OllamaNative;
    }
    Dialect::Passthrough
}

/// Classifies every inference request into an exact safe transformation profile so
/// non-agentic single-turn RAG/chat and structured JSON requests never receive agentic injections.
pub fn classify_workload(dialect: Dialect, root: &Value, active_tool_loop: bool) -> WorkloadClass {
    if dialect == Dialect::Passthrough {
        return WorkloadClass::Passthrough;
    }
    if is_structured_output(root) {
        return WorkloadClass::StructuredOutput;
    }
    if active_tool_loop || has_agentic_tool_history(root) {
        return WorkloadClass::AgenticLoop;
    }
    if has_large_fenced_code(root) {
        return WorkloadClass::DirectCodeEdit;
    }
    WorkloadClass::SingleTurnRagOrChat
}

/// Returns true if the request enforces a JSON / schema structured output constraint.
pub fn is_structured_output(root: &Value) -> bool {
    if let Some(rf) = root.get("response_format") {
        let rf_type = rf.get("type").and_then(Value::as_str).unwrap_or_default();
        if rf_type == "json_object" || rf_type == "json_schema" {
            return true;
        }
    }
    if let Some(fmt_type) = root.pointer("/text/format/type").and_then(Value::as_str) {
        if fmt_type == "json_schema" || fmt_type == "json_object" {
            return true;
        }
    }
    if let Some(gc) = root.get("generationConfig") {
        if gc.get("responseSchema").is_some()
            || gc.get("responseMimeType").and_then(Value::as_str) == Some("application/json")
        {
            return true;
        }
    }
    root.get("format").and_then(Value::as_str) == Some("json")
}

/// Returns true if the request path is a non-inference endpoint that must pass through untouched.
pub fn is_passthrough_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    const BYPASS_SEGMENTS: &[&str] = &[
        "/models",
        "/embeddings",
        "/audio/",
        "/images/",
        "/files",
        "/moderations",
        "/batches",
        "/fine_tuning",
        "/api/tags",
        "/api/show",
        "/api/ps",
        "/api/pull",
        "/count_tokens",
        "/tokenize",
    ];
    BYPASS_SEGMENTS.iter().any(|seg| lower.contains(seg))
}

fn has_agentic_tool_history(root: &Value) -> bool {
    if let Some(items) = root.get("input").and_then(Value::as_array) {
        if items.iter().any(|i| {
            matches!(
                i.get("type").and_then(Value::as_str),
                Some("function_call" | "function_call_output")
            )
        }) {
            return true;
        }
    }
    let Some(msgs) = root.get("messages").and_then(Value::as_array) else {
        return false;
    };
    msgs.iter().any(|m| {
        m.get("role").and_then(Value::as_str) == Some("tool")
            || m.get("tool_calls")
                .and_then(Value::as_array)
                .map(|a| !a.is_empty())
                .unwrap_or(false)
            || m.get("content")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter().any(|b| {
                        matches!(
                            b.get("type").and_then(Value::as_str),
                            Some("tool_use" | "tool_result")
                        )
                    })
                })
                .unwrap_or(false)
    })
}

fn has_large_fenced_code(root: &Value) -> bool {
    let Some(text) = extract_latest_user_text(root) else {
        return false;
    };
    let Some(start) = text.find("```") else {
        return false;
    };
    let Some(nl) = text[start + 3..].find('\n') else {
        return false;
    };
    let body_start = start + 3 + nl + 1;
    let Some(end) = text[body_start..].find("```") else {
        return false;
    };
    text[body_start..body_start + end].lines().count() > 40
}

fn extract_latest_user_text(root: &Value) -> Option<&str> {
    if let Some(msgs) = root.get("messages").and_then(Value::as_array) {
        return msgs.last()?.get("content")?.as_str();
    }
    if let Some(prompt) = root.get("prompt").and_then(Value::as_str) {
        return Some(prompt);
    }
    if let Some(input) = root.get("input").and_then(Value::as_str) {
        return Some(input);
    }
    None
}
