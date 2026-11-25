use aphelion_core::stream_fsm::StreamFsm;
use aphelion_core::types::{ExecutionProfile, SessionState};
use serde_json::{json, Value};

#[test]
fn sliding_cache_control_preserved_and_system_consistent_across_turns() {
    let mut state = SessionState::default();
    let file_body = (1..=45)
        .map(|i| format!("{i:>6}→def fn_{i}(x): return x + {i}"))
        .collect::<Vec<_>>()
        .join("\n");

    let base_sys = json!([
        {"type": "text", "text": "You are Claude Code.", "cache_control": {"type": "ephemeral"}}
    ]);
    let tools = json!([
        {"name": "Read", "description": "Read file", "input_schema": {"type": "object"}},
        {"name": "Write", "description": "Write file", "input_schema": {"type": "object"}},
        {"name": "Edit", "description": "Edit file", "input_schema": {"type": "object"}},
        {"name": "Bash", "description": "Run shell", "input_schema": {"type": "object"}}
    ]);

    // Turn 0: initial user prompt + first Read result (no cache_control on messages[2] yet)
    let t0 = json!({
        "anthropic_version": "2023-06-01",
        "model": "claude-sonnet-4-5",
        "max_tokens": 2048,
        "system": base_sys,
        "tools": tools,
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "Build cache.py and test it"}]},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "tu_1", "name": "Read", "input": {"file_path": "cache.py"}}
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "tu_1", "content": [{"type": "text", "text": file_body}]}
            ]}
        ]
    });
    let out0 = aphelion_core::prepare_request(
        &serde_json::to_vec(&t0).unwrap(),
        ExecutionProfile::Ephemeral,
        &mut state,
    )
    .unwrap();
    let p0: Value = serde_json::from_slice(&out0.body).unwrap();
    assert_eq!(p0["system"].as_array().unwrap().len(), 2);
    // Must NEVER inject blocking tool-scope directive on Anthropic sessions.
    assert!(!String::from_utf8_lossy(&out0.body).contains("Active Tool Scope"));
    assert!(!String::from_utf8_lossy(&out0.body).contains("ignore all other tools"));

    // Turn 1: Claude Code slides cache_control onto messages[2] and repeats Read of cache.py in messages[4].
    let t1 = json!({
        "anthropic_version": "2023-06-01",
        "model": "claude-sonnet-4-5",
        "max_tokens": 2048,
        "system": base_sys,
        "tools": tools,
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "Build cache.py and test it"}]},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "tu_1", "name": "Read", "input": {"file_path": "cache.py"}}
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "tu_1", "content": [{"type": "text", "text": file_body}], "cache_control": {"type": "ephemeral"}}
            ]},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "tu_2", "name": "Read", "input": {"file_path": "cache.py"}}
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "tu_2", "content": [{"type": "text", "text": file_body}]}
            ]}
        ]
    });
    let out1 = aphelion_core::prepare_request(
        &serde_json::to_vec(&t1).unwrap(),
        ExecutionProfile::Ephemeral,
        &mut state,
    )
    .unwrap();
    let p1: Value = serde_json::from_slice(&out1.body).unwrap();

    // 1. System array stays 100% byte-identical between Turn 0 and Turn 1 (zero cache break!).
    assert_eq!(
        serde_json::to_vec(&p0["system"]).unwrap(),
        serde_json::to_vec(&p1["system"]).unwrap()
    );
    // 2. Sliding cache_control on messages[2] MUST survive PrefixLedger replay!
    assert_eq!(
        p1["messages"][2]["content"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    // 3. Repeated Read in messages[4] is deduplicated!
    let dedup = p1["messages"][4]["content"][0]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(dedup.starts_with("[Aphelion Ingress Dedup: `cache.py`"));

    // Turn 2: Claude Code slides cache_control off messages[2] onto messages[4].
    let mut t2 = t1.clone();
    t2["messages"][2]["content"][0]
        .as_object_mut()
        .unwrap()
        .remove("cache_control");
    t2["messages"][4]["content"][0]["cache_control"] = json!({"type": "ephemeral"});
    t2["messages"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role": "assistant", "content": [{"type": "text", "text": "Audited."}]}));
    t2["messages"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role": "user", "content": [{"type": "text", "text": "Finish."}]}));

    let out2 = aphelion_core::prepare_request(
        &serde_json::to_vec(&t2).unwrap(),
        ExecutionProfile::Ephemeral,
        &mut state,
    )
    .unwrap();
    let p2: Value = serde_json::from_slice(&out2.body).unwrap();
    // Retired cache_control on messages[2] is removed (stays <= 4 breakpoints) and new one on messages[4] is kept!
    assert!(p2["messages"][2]["content"][0].get("cache_control").is_none());
    assert_eq!(
        p2["messages"][4]["content"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    // Deduped content in messages[4] stays frozen verbatim!
    assert!(p2["messages"][4]["content"][0]["content"][0]["text"]
        .as_str()
        .unwrap()
        .starts_with("[Aphelion Ingress Dedup: `cache.py`"));
}

#[test]
fn sse_fsm_handles_crlf_frame_separators() {
    let mut fsm = StreamFsm::default();
    let mut session = SessionState::default();
    let wire = b"event: message_start\r\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":500,\"cache_read_input_tokens\":400}}}\r\n\r\nevent: message_stop\r\ndata: {\"type\":\"message_stop\"}\r\n\r\n";
    let out = fsm.process_sse_chunk(wire, &mut session);
    assert!(!out.is_empty());
    let u = session.last_usage.expect("usage extracted from CRLF SSE");
    assert_eq!(u.cached_tokens, 400);
}
