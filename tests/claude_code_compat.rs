use aphelion_cli::headers::UpstreamTargets;
use aphelion_cli::router::AutoRouter;
use aphelion_cli::spawn::detect_claude_settings_base_url;
use aphelion_core::canonicalizer::verify_prefix_invariant;
use aphelion_core::types::{CompactMode, ExecutionProfile, SessionState};
use hyper::header::HeaderValue;
use hyper::HeaderMap;
use serde_json::{json, Value};

fn sample_claude_read_body() -> String {
    (1..=45)
        .map(|i| format!("{i:>6}→pub fn step_{i}(x: usize) -> usize {{ x.wrapping_add({i}) }}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn cache_control_invariance_and_post_breakpoint_optimization() {
    let mut state = SessionState::default();
    let file_body = sample_claude_read_body();
    let noisy_bash = "compiling aphelion-core v3.3.0 check ok\n".repeat(12);
    let tron_rows = json!([
        {"id": 1, "file": "src/a.rs", "status": "ok"},
        {"id": 2, "file": "src/b.rs", "status": "ok"},
        {"id": 3, "file": "src/c.rs", "status": "fail"}
    ])
    .to_string();

    let payload = json!({
        "anthropic_version": "2023-06-01",
        "model": "claude-sonnet-4-5",
        "max_tokens": 4096,
        "system": [
            {"type": "text", "text": "You are Claude Code.", "cache_control": {"type": "ephemeral"}}
        ],
        "tools": [
            {"name": "Read", "description": "Read a file", "input_schema": {"type": "object"}},
            {"name": "Bash", "description": "Run shell command", "input_schema": {"type": "object"}}
        ],
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "Inspect lib.rs", "cache_control": {"type": "ephemeral"}}]},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {"file_path": "crates/core/src/lib.rs"}}
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_1", "content": file_body, "cache_control": {"type": "ephemeral"}}
            ]},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "toolu_2", "name": "Read", "input": {"file_path": "crates/core/src/lib.rs"}},
                {"type": "tool_use", "id": "toolu_3", "name": "Bash", "input": {"command": "cargo check"}},
                {"type": "tool_use", "id": "toolu_4", "name": "Bash", "input": {"command": "list-json"}}
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_2", "content": file_body},
                {"type": "tool_result", "tool_use_id": "toolu_3", "content": noisy_bash},
                {"type": "tool_result", "tool_use_id": "toolu_4", "content": tron_rows}
            ]}
        ]
    });

    let raw = serde_json::to_vec(&payload).unwrap();
    let out = aphelion_core::prepare_request(&raw, ExecutionProfile::Ephemeral, &mut state).unwrap();
    let parsed: Value = serde_json::from_slice(&out.body).unwrap();

    // 1. messages[0..=2] up to and including last cache_control breakpoint are 100% byte-identical.
    for idx in 0..=2 {
        assert_eq!(
            serde_json::to_vec(&parsed["messages"][idx]).unwrap(),
            serde_json::to_vec(&payload["messages"][idx]).unwrap(),
            "cached message {idx} must remain byte-identical"
        );
    }

    // 2. Post-breakpoint tool_results at messages[4] are deduplicated, log-compressed, and TRON-encoded.
    let post = parsed["messages"][4]["content"].as_array().unwrap();
    assert!(post[0]["content"]
        .as_str()
        .unwrap()
        .starts_with("[Aphelion Ingress Dedup: `crates/core/src/lib.rs`"));
    assert!(post[1]["content"]
        .as_str()
        .unwrap()
        .contains("repeated 11 more times"));
    assert!(post[2]["content"].as_str().unwrap().starts_with("@tron["));
    assert!(out.metrics.stage_stats.ingress_saved_tokens > 0);
    assert_eq!(out.metrics.stage_stats.tron_tables_encoded, 1);

    // 3. Macro-compaction still triggers on Anthropic payloads under Force mode and rolls over epoch.
    state.request_options.compact_mode = CompactMode::Force;
    let mut long_msgs = payload["messages"].as_array().unwrap().clone();
    for i in 0..6 {
        long_msgs.push(json!({"role": "assistant", "content": format!("Step {i}")}));
        long_msgs.push(json!({"role": "user", "content": format!("Ack {i}")}));
    }
    let mut compact_req = payload.clone();
    compact_req["messages"] = Value::Array(long_msgs);
    let compact_raw = serde_json::to_vec(&compact_req).unwrap();
    let compact_out =
        aphelion_core::prepare_request(&compact_raw, ExecutionProfile::Ephemeral, &mut state)
            .unwrap();
    assert!(compact_out.metrics.stage_stats.macro_compaction_triggered);
}

#[test]
fn tool_use_id_to_tool_result_array_dedup() {
    let mut state = SessionState::default();
    let file_body = sample_claude_read_body();

    let t0 = json!({
        "anthropic_version": "2023-06-01",
        "model": "claude-sonnet-4-5",
        "max_tokens": 2048,
        "system": [{"type": "text", "text": "Claude Code", "cache_control": {"type": "ephemeral"}}],
        "tools": [{"name": "Read", "description": "Read file", "input_schema": {"type": "object"}}],
        "messages": [
            {"role": "user", "content": "Read src/engine.rs"},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "tu_a", "name": "Read", "input": {"file_path": "src/engine.rs"}}
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "tu_a", "content": [{"type": "text", "text": file_body}]}
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
    assert_eq!(p0["messages"][2]["content"][0]["content"][0]["text"], file_body);

    let mut t1 = t0.clone();
    t1["messages"].as_array_mut().unwrap().push(json!({
        "role": "assistant",
        "content": [{"type": "tool_use", "id": "tu_b", "name": "Read", "input": {"file_path": "src/engine.rs"}}]
    }));
    t1["messages"].as_array_mut().unwrap().push(json!({
        "role": "user",
        "content": [{"type": "tool_result", "tool_use_id": "tu_b", "content": [{"type": "text", "text": file_body}]}]
    }));

    let out1 = aphelion_core::prepare_request(
        &serde_json::to_vec(&t1).unwrap(),
        ExecutionProfile::Ephemeral,
        &mut state,
    )
    .unwrap();
    let p1: Value = serde_json::from_slice(&out1.body).unwrap();
    let dedup_text = p1["messages"][4]["content"][0]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(dedup_text.starts_with("[Aphelion Ingress Dedup: `src/engine.rs`"));
    assert!(out1.metrics.stage_stats.ingress_saved_tokens > 100);
}

#[test]
fn array_system_output_steering_preserves_cached_blocks() {
    let mut state = SessionState::default();
    let req = json!({
        "anthropic_version": "2023-06-01",
        "model": "claude-sonnet-4-5",
        "max_tokens": 1024,
        "system": [
            {"type": "text", "text": "Base system prompt.", "cache_control": {"type": "ephemeral"}}
        ],
        "messages": [{"role": "user", "content": "Hello"}]
    });
    let raw = serde_json::to_vec(&req).unwrap();
    let out = aphelion_core::prepare_request(&raw, ExecutionProfile::Ephemeral, &mut state).unwrap();
    let parsed: Value = serde_json::from_slice(&out.body).unwrap();

    let sys = parsed["system"].as_array().unwrap();
    assert_eq!(sys.len(), 2);
    assert_eq!(sys[0], req["system"][0]);
    assert_eq!(sys[1]["type"], "text");
    assert!(sys[1]["text"].as_str().unwrap().contains("[Output Policy:"));
    assert!(state.system_has_cache_control);
    assert!(verify_prefix_invariant(&raw, &out.body, state.frozen_prefix_len));
}

#[test]
fn api_hello_not_routed_to_ollama_and_claude_settings_detected() {
    let targets = UpstreamTargets {
        openai: "https://api.openai.com/v1".to_owned(),
        anthropic: "http://localhost:4000".to_owned(),
        gemini: "https://generativelanguage.googleapis.com".to_owned(),
        ollama: "http://127.0.0.1:11434".to_owned(),
    };

    // `/api/hello` and `/api/event_logging/batch` must NEVER be routed to Ollama.
    let mut anth_headers = HeaderMap::new();
    anth_headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
    let r_hello = AutoRouter::resolve(&anth_headers, "/api/hello", None, &targets, false).unwrap();
    assert_ne!(r_hello.provider_id, "ollama");
    assert_eq!(r_hello.provider_id, "anthropic");
    assert_eq!(r_hello.target_url, "http://localhost:4000/api/hello");

    let r_plain = AutoRouter::resolve(&HeaderMap::new(), "/api/hello", None, &targets, false).unwrap();
    assert_ne!(r_plain.provider_id, "ollama");

    // Generic LiteLLM `sk-...` virtual keys on `/v1/messages` MUST route to Anthropic, never OpenAI.
    let mut litellm_headers = HeaderMap::new();
    litellm_headers.insert("authorization", HeaderValue::from_static("Bearer sk-litellm-virtual-123"));
    let r_litellm = AutoRouter::resolve(&litellm_headers, "/v1/messages?beta=true", None, &targets, false).unwrap();
    assert_eq!(r_litellm.provider_id, "anthropic");
    assert_eq!(r_litellm.target_url, "http://localhost:4000/v1/messages?beta=true");

    // Valid Ollama endpoints MUST still route to Ollama.
    for path in [
        "/api/chat",
        "/api/generate",
        "/api/tags",
        "/api/embed",
        "/api/embeddings",
        "/api/show",
        "/api/ps",
        "/api/version",
    ] {
        let r = AutoRouter::resolve(&HeaderMap::new(), path, None, &targets, false).unwrap();
        assert_eq!(r.provider_id, "ollama", "expected {path} -> ollama");
    }

    // Claude settings.json guard detects env.ANTHROPIC_BASE_URL cleanly.
    let tmp_home = std::env::temp_dir().join("aphelion_claude_settings_guard_test");
    let _ = std::fs::create_dir_all(tmp_home.join(".claude"));
    std::fs::write(
        tmp_home.join(".claude").join("settings.json"),
        r#"{"env":{"ANTHROPIC_BASE_URL":"http://localhost:4000"}}"#,
    )
    .unwrap();
    assert_eq!(
        detect_claude_settings_base_url(&tmp_home).as_deref(),
        Some("http://localhost:4000")
    );
    let _ = std::fs::remove_dir_all(&tmp_home);
}
