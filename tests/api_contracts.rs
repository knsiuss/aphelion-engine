use aphelion_core::types::{Dialect, ExecutionProfile, SessionState};
use aphelion_core::validator::validate_outbound_messages;
use serde_json::{json, Value};

#[test]
fn openai_diet_gc_pins_actively_referenced_file_and_keeps_valid_graph() {
    let mut state = SessionState::default();
    let big_old = "pub fn legacy() { let x = 1; }\n".repeat(450);
    let active_src = "pub fn active_target() -> usize { 42 }\n".repeat(350);

    // Turn 0
    let t0 = serde_json::to_vec(&json!({
        "model": "gpt-4o",
        "system": "Rust assistant cwd: /repo/app",
        "messages": [{"role": "user", "content": "Inspect files"}]
    }))
    .unwrap();
    aphelion_core::prepare_request(&t0, ExecutionProfile::Ephemeral, &mut state).unwrap();

    // Turn 1: old_file.rs read (stale) + active.rs read (referenced in final user turn)
    let t1 = serde_json::to_vec(&json!({
        "model": "gpt-4o",
        "system": "Rust assistant cwd: /repo/app",
        "messages": [
            {"role": "user", "content": "Inspect files"},
            {"role": "assistant", "content": "Reading old", "tool_calls": [{"id": "c1", "function": {"name": "read_file", "arguments": "{\"path\":\"src/old_file.rs\"}"}}]},
            {"role": "tool", "tool_call_id": "c1", "name": "read_file", "content": format!("File: src/old_file.rs\n{big_old}")},
            {"role": "assistant", "content": "Reading active", "tool_calls": [{"id": "c2", "function": {"name": "read_file", "arguments": "{\"path\":\"src/active.rs\"}"}}]},
            {"role": "tool", "tool_call_id": "c2", "name": "read_file", "content": format!("File: src/active.rs\n{active_src}")},
            {"role": "user", "content": "Now update src/active.rs to return 99."}
        ]
    }))
    .unwrap();

    let out = aphelion_core::prepare_request(&t1, ExecutionProfile::Ephemeral, &mut state).unwrap();
    let parsed: Value = serde_json::from_slice(&out.body).unwrap();
    assert!(validate_outbound_messages(Dialect::OpenAiChat, &parsed).is_ok());
    assert!(!out.metrics.stage_stats.validator_rollback);

    let msgs = parsed["messages"].as_array().unwrap();
    // Stale old_file.rs was compacted, while actively referenced src/active.rs was pinned!
    assert!(msgs[2]["content"].as_str().unwrap().starts_with("[AgentDiet Tombstone:"));
    assert!(msgs[4]["content"].as_str().unwrap().contains("pub fn active_target()"));
}

#[test]
fn anthropic_atomic_pair_collapse_preserves_role_alternation_and_tool_ids() {
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

    let out1 = aphelion_core::prepare_request(&make_req(1), ExecutionProfile::Ephemeral, &mut state).unwrap();
    let p1: Value = serde_json::from_slice(&out1.body).unwrap();
    assert!(validate_outbound_messages(Dialect::AnthropicMessages, &p1).is_ok());

    let out2 = aphelion_core::prepare_request(&make_req(2), ExecutionProfile::Ephemeral, &mut state).unwrap();
    let p2: Value = serde_json::from_slice(&out2.body).unwrap();
    assert!(out2.metrics.stage_stats.circuit_breaker_tripped);
    assert!(!out2.metrics.stage_stats.validator_rollback);
    assert!(validate_outbound_messages(Dialect::AnthropicMessages, &p2).is_ok());
    // Atomic pair collapse removed (tu_1 assistant + tu_1 user), leaving user + tu_2 pair = 3 msgs
    assert_eq!(p2["messages"].as_array().unwrap().len(), 3);
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
