use aphelion_core::types::{Dialect, ExecutionProfile, SessionState};
use aphelion_core::validator::validate_outbound_messages;
use serde_json::{json, Value};

#[test]
fn openai_ingress_dedup_preserves_initial_read_and_valid_graph() {
    let mut state = SessionState::default();
    let big_file = format!(
        "File: src/old_file.rs\n{}",
        "pub fn legacy() { let x = 1; }\n".repeat(45)
    );

    let t0 = serde_json::to_vec(&json!({
        "model": "gpt-4o",
        "system": "Rust assistant cwd: /repo/app",
        "messages": [
            {"role": "user", "content": "Inspect files"},
            {"role": "assistant", "content": "Reading", "tool_calls": [{"id": "c1", "function": {"name": "read_file", "arguments": "{\"path\":\"src/old_file.rs\"}"}}]},
            {"role": "tool", "tool_call_id": "c1", "name": "read_file", "content": big_file}
        ]
    }))
    .unwrap();
    let out0 =
        aphelion_core::prepare_request(&t0, ExecutionProfile::Ephemeral, &mut state).unwrap();

    // Turn 1: re-reads identical src/old_file.rs at ingress -> deduplicated BEFORE freeze,
    // while frozen Turn-0 history remains 100% immutable and graph stays valid.
    let t1 = serde_json::to_vec(&json!({
        "model": "gpt-4o",
        "system": "Rust assistant cwd: /repo/app",
        "messages": [
            {"role": "user", "content": "Inspect files"},
            {"role": "assistant", "content": "Reading", "tool_calls": [{"id": "c1", "function": {"name": "read_file", "arguments": "{\"path\":\"src/old_file.rs\"}"}}]},
            {"role": "tool", "tool_call_id": "c1", "name": "read_file", "content": big_file},
            {"role": "assistant", "content": "Reading again", "tool_calls": [{"id": "c2", "function": {"name": "read_file", "arguments": "{\"path\":\"src/old_file.rs\"}"}}]},
            {"role": "tool", "tool_call_id": "c2", "name": "read_file", "content": big_file}
        ]
    }))
    .unwrap();

    let out1 =
        aphelion_core::prepare_request(&t1, ExecutionProfile::Ephemeral, &mut state).unwrap();
    let parsed: Value = serde_json::from_slice(&out1.body).unwrap();
    assert!(validate_outbound_messages(Dialect::OpenAiChat, &parsed).is_ok());
    assert!(!out1.metrics.stage_stats.validator_rollback);
    assert_eq!(
        &out1.body[..out0.body.len() - 2],
        &out0.body[..out0.body.len() - 2]
    );

    let msgs = parsed["messages"].as_array().unwrap();
    assert!(msgs[2]["content"]
        .as_str()
        .unwrap()
        .contains("pub fn legacy()"));
    assert!(msgs[4]["content"]
        .as_str()
        .unwrap()
        .starts_with("[Aphelion Ingress Dedup:"));
}

#[test]
fn anthropic_circuit_breaker_preserves_history_and_tool_ids() {
    let mut state = SessionState::default();
    let make_req = |retry_count: usize| {
        let mut msgs = vec![json!({"role": "user", "content": "Fix schema.sql"})];
        for i in 1..=retry_count {
            let id = format!("tu_{i}");
            msgs.push(json!({
                "role": "assistant",
                "content": [{"type": "tool_use", "id": id, "name": "edit_file", "input": {"path": "schema.sql"}}]
            }));
            msgs.push(json!({
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": id, "content": "Error: permission denied on schema.sql FAILED"}]
            }));
        }
        serde_json::to_vec(&json!({
            "anthropic_version": "2023-06-01",
            "max_tokens": 1024,
            "system": "Anthropic agent cwd: /repo/app",
            "tools": [{"name": "edit_file", "description": "Edit file"}],
            "messages": msgs
        }))
        .unwrap()
    };

    let out1 =
        aphelion_core::prepare_request(&make_req(1), ExecutionProfile::Ephemeral, &mut state)
            .unwrap();
    let p1: Value = serde_json::from_slice(&out1.body).unwrap();
    assert!(validate_outbound_messages(Dialect::AnthropicMessages, &p1).is_ok());

    let out2 =
        aphelion_core::prepare_request(&make_req(2), ExecutionProfile::Ephemeral, &mut state)
            .unwrap();
    let p2: Value = serde_json::from_slice(&out2.body).unwrap();
    assert!(out2.metrics.stage_stats.circuit_breaker_tripped);
    assert!(!out2.metrics.stage_stats.validator_rollback);
    assert!(validate_outbound_messages(Dialect::AnthropicMessages, &p2).is_ok());
    // Append-Only Prefix Invariance: all 5 messages remain intact and frozen prefix matches out1.
    assert_eq!(p2["messages"].as_array().unwrap().len(), 5);
    assert_eq!(
        &out2.body[..out1.body.len() - 2],
        &out1.body[..out1.body.len() - 2]
    );
}

#[test]
fn validator_catches_orphaned_tool_ids_and_consecutive_roles() {
    let bad_openai = json!({
        "messages": [
            {"role": "user", "content": "hi"},
            {"role": "tool", "tool_call_id": "orphan_1", "content": "oops"}
        ]
    });
    assert!(validate_outbound_messages(Dialect::OpenAiChat, &bad_openai).is_err());

    let bad_anthropic = json!({
        "messages": [
            {"role": "user", "content": "hi"},
            {"role": "user", "content": "consecutive user"}
        ]
    });
    assert!(validate_outbound_messages(Dialect::AnthropicMessages, &bad_anthropic).is_err());
}
