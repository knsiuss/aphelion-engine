use aphelion_core::types::{ExecutionProfile, SessionState, UpstreamUsage};
use aphelion_core::usage_extract::extract_usage_universal;
use aphelion_telemetry::audit::{AuditLedger, AuditRecord};
use serde_json::{json, Value};

#[test]
fn fifteen_turn_append_only_prefix_byte_invariance() {
    let mut state = SessionState::default();
    let tools = json!([
        {"type": "function", "function": {"name": "write_file", "description": "Write file"}},
        {"type": "function", "function": {"name": "read_file", "description": "Read file"}}
    ]);
    let mut history: Vec<Value> = vec![json!({"role": "user", "content": "Build feature X."})];
    let mut prev_wire: Option<Vec<u8>> = None;

    for turn in 0..15usize {
        if turn > 0 {
            let call_id = format!("call_{turn}");
            history.push(json!({
                "role": "assistant",
                "content": format!("<think>Analyzing step {turn}.</think>\nReading file."),
                "tool_calls": [{"id": call_id, "function": {"name": "read_file", "arguments": "{\"path\":\"src/lib.rs\"}"}}]
            }));
            history.push(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "name": "read_file",
                "content": format!("File: src/lib.rs\npub fn step_{turn}() -> usize {{ {turn} }}\n{}", "let x = 1;\n".repeat(45))
            }));
        }

        let raw = serde_json::to_vec(&json!({
            "model": "gpt-4o",
            "system": "You are a Rust systems engineer. cwd: /repo/aphelion Time: 2026-11-24T21:00:00Z",
            "tools": tools,
            "messages": history
        }))
        .unwrap();

        let out =
            aphelion_core::prepare_request(&raw, ExecutionProfile::Ephemeral, &mut state).unwrap();
        if let Some(prev) = &prev_wire {
            // Every turn appends to `messages[]`; serialized bytes up to the closing `]}`
            // of the previous turn must be 100% byte-for-byte identical.
            let prev_prefix_end = prev.len() - 2;
            assert_eq!(
                &out.body[..prev_prefix_end],
                &prev[..prev_prefix_end],
                "prefix mismatch at turn {turn}"
            );
        }
        prev_wire = Some(out.body);
    }
}

#[test]
fn ingress_optimization_freezes_before_first_dispatch() {
    let mut state = SessionState::default();
    let arr_json = json!([
        {"id": 1, "file": "a.rs", "ok": true},
        {"id": 2, "file": "b.rs", "ok": true},
        {"id": 3, "file": "c.rs", "ok": false}
    ])
    .to_string();
    let file_body = format!("File: src/app.rs\n{}", "pub fn keep() {}\n".repeat(30));

    let t0 = serde_json::to_vec(&json!({
        "model": "gpt-4o",
        "system": "Agent cwd: /repo/app",
        "tools": [{"type": "function", "function": {"name": "read_file"}}],
        "messages": [
            {"role": "user", "content": "Inspect"},
            {"role": "assistant", "content": "Reading", "tool_calls": [{"id": "c1", "function": {"name": "read_file", "arguments": "{}"}}]},
            {"role": "tool", "tool_call_id": "c1", "name": "read_file", "content": file_body}
        ]
    }))
    .unwrap();
    let out0 =
        aphelion_core::prepare_request(&t0, ExecutionProfile::Ephemeral, &mut state).unwrap();

    // Turn 1 sends identical file read + TRON array + noisy log; ingress optimizes them once.
    let noisy_log = "checking status ok\n".repeat(6);
    let t1 = serde_json::to_vec(&json!({
        "model": "gpt-4o",
        "system": "Agent cwd: /repo/app",
        "tools": [{"type": "function", "function": {"name": "read_file"}}],
        "messages": [
            {"role": "user", "content": "Inspect"},
            {"role": "assistant", "content": "Reading", "tool_calls": [{"id": "c1", "function": {"name": "read_file", "arguments": "{}"}}]},
            {"role": "tool", "tool_call_id": "c1", "name": "read_file", "content": file_body},
            {"role": "assistant", "content": "Re-reading and searching", "tool_calls": [
                {"id": "c2", "function": {"name": "read_file", "arguments": "{}"}},
                {"id": "c3", "function": {"name": "grep", "arguments": "{}"}},
                {"id": "c4", "function": {"name": "log", "arguments": "{}"}}
            ]},
            {"role": "tool", "tool_call_id": "c2", "name": "read_file", "content": file_body},
            {"role": "tool", "tool_call_id": "c3", "name": "grep", "content": arr_json},
            {"role": "tool", "tool_call_id": "c4", "name": "log", "content": noisy_log}
        ]
    }))
    .unwrap();
    let out1 =
        aphelion_core::prepare_request(&t1, ExecutionProfile::Ephemeral, &mut state).unwrap();
    assert_eq!(
        &out1.body[..out0.body.len() - 2],
        &out0.body[..out0.body.len() - 2]
    );
    let p1: Value = serde_json::from_slice(&out1.body).unwrap();
    assert!(p1["messages"][4]["content"]
        .as_str()
        .unwrap()
        .starts_with("[Aphelion Ingress Dedup:"));
    assert!(p1["messages"][5]["content"]
        .as_str()
        .unwrap()
        .starts_with("@tron["));
    assert!(p1["messages"][6]["content"]
        .as_str()
        .unwrap()
        .contains("repeated 5 more times"));

    // Turn 2 replays Turn 1's exact compressed frozen bytes verbatim.
    let t2 = serde_json::to_vec(&json!({
        "model": "gpt-4o",
        "system": "Agent cwd: /repo/app",
        "tools": [{"type": "function", "function": {"name": "read_file"}}],
        "messages": [
            {"role": "user", "content": "Inspect"},
            {"role": "assistant", "content": "Reading", "tool_calls": [{"id": "c1", "function": {"name": "read_file", "arguments": "{}"}}]},
            {"role": "tool", "tool_call_id": "c1", "name": "read_file", "content": file_body},
            {"role": "assistant", "content": "Re-reading and searching", "tool_calls": [
                {"id": "c2", "function": {"name": "read_file", "arguments": "{}"}},
                {"id": "c3", "function": {"name": "grep", "arguments": "{}"}},
                {"id": "c4", "function": {"name": "log", "arguments": "{}"}}
            ]},
            {"role": "tool", "tool_call_id": "c2", "name": "read_file", "content": file_body},
            {"role": "tool", "tool_call_id": "c3", "name": "grep", "content": arr_json},
            {"role": "tool", "tool_call_id": "c4", "name": "log", "content": noisy_log},
            {"role": "user", "content": "Continue"}
        ]
    }))
    .unwrap();
    let out2 =
        aphelion_core::prepare_request(&t2, ExecutionProfile::Ephemeral, &mut state).unwrap();
    assert_eq!(
        &out2.body[..out1.body.len() - 2],
        &out1.body[..out1.body.len() - 2]
    );
}

#[test]
fn cache_drop_circuit_breaker_locks_frozen_prefix_pass() {
    let mut state = SessionState::default();
    let t0 = serde_json::to_vec(&json!({
        "model": "gpt-4o",
        "system": "Sys",
        "tools": [{"type": "function", "function": {"name": "grep"}}],
        "messages": [{"role": "user", "content": "Hello"}]
    }))
    .unwrap();
    aphelion_core::prepare_request(&t0, ExecutionProfile::Ephemeral, &mut state).unwrap();

    // Feed 3 turns with cache_hit_ratio = 200 / 1000 = 0.20 (< 0.70 threshold)
    for _ in 0..3 {
        state.record_upstream_usage(&UpstreamUsage {
            input_tokens: 1_000,
            uncached_input_tokens: 800,
            cached_tokens: 200,
            ..UpstreamUsage::default()
        });
    }

    let arr = json!([{"a": 1}, {"a": 2}, {"a": 3}]).to_string();
    let t1 = serde_json::to_vec(&json!({
        "model": "gpt-4o",
        "system": "Sys",
        "tools": [{"type": "function", "function": {"name": "grep"}}],
        "messages": [
            {"role": "user", "content": "Hello"},
            {"role": "assistant", "content": "Calling", "tool_calls": [{"id": "1", "function": {"name": "grep", "arguments": "{}"}}]},
            {"role": "tool", "tool_call_id": "1", "name": "grep", "content": arr}
        ]
    }))
    .unwrap();
    let out = aphelion_core::prepare_request(&t1, ExecutionProfile::Ephemeral, &mut state).unwrap();
    assert!(out.metrics.stage_stats.cache_drop_breaker_active);
    let parsed: Value = serde_json::from_slice(&out.body).unwrap();
    // FrozenPrefixPass: TRON skipped so raw JSON array passes untouched.
    assert_eq!(parsed["messages"][2]["content"].as_str().unwrap(), arr);
}

#[test]
fn deepseek_usage_and_asymmetric_audit_accounting() {
    let ds = json!({
        "usage": {
            "prompt_tokens": 1000,
            "completion_tokens": 100,
            "prompt_cache_hit_tokens": 900,
            "prompt_cache_miss_tokens": 100
        }
    });
    let u = extract_usage_universal(&ds).unwrap();
    assert_eq!(u.cached_tokens, 900);
    assert_eq!(u.uncached_input_tokens, 100);

    // Regression case: proxy caused high uncached input on turn 3 -> net_usd_saved must be <= 0.
    let ledger = AuditLedger::open_in_memory().unwrap();
    ledger
        .record(&AuditRecord {
            session_id: "s1".to_owned(),
            turn_index: 3,
            model: "gpt-4o".to_owned(),
            raw_tokens: 10_000,
            opt_tokens: 9_000,
            uncached_input_tokens: 9_000,
            cache_read_tokens: 0,
            output_tokens: 100,
            ..AuditRecord::default()
        })
        .unwrap();
    let summary = ledger.summarize().unwrap();
    assert!(summary.net_usd_saved() < 0.0);
}
