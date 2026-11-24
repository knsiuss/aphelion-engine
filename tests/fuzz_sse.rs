use aphelion_core::canonicalizer::verify_prefix_invariant;
use aphelion_core::compressor::compress_with_syntax_mask;
use aphelion_core::diet_gc::{estimate_calibrated_tokens, CACHE_AMORTIZATION_WATERMARK_TOKENS};
use aphelion_core::stream_fsm::StreamFsm;
use aphelion_core::tron_codec::{decode_tron_table, try_encode_tron_table};
use aphelion_core::types::{BlockSpan, CodeLang, ExecutionProfile, SessionState};
use serde_json::{json, Value};

fn feed_chunked_sse(raw_sse: &[u8], chunk_sizes: &[usize], session: &mut SessionState) -> Vec<u8> {
    let mut fsm = StreamFsm::default();
    let mut out = Vec::new();
    let mut offset = 0usize;
    let mut idx = 0usize;
    while offset < raw_sse.len() {
        let sz = chunk_sizes[idx % chunk_sizes.len()].max(1);
        let end = (offset + sz).min(raw_sse.len());
        out.extend_from_slice(&fsm.process_sse_chunk(&raw_sse[offset..end], session));
        offset = end;
        idx += 1;
    }
    out.extend_from_slice(&fsm.flush_with_session(session));
    out
}

#[test]
fn sse_one_byte_and_random_fragmentation_matches_whole_stream() {
    let frames = [
        json!({"choices":[{"delta":{"content":"<think>Checking UTF-8 🚀🔥🦀 日本語. Verified exact fix.</think>"}}]}),
        json!({"choices":[{"delta":{"content":"Applied patch cleanly 🚀"}}]}),
        json!({"choices":[{"delta":{"tool_calls":[{"id":"call_1","function":{"name":"edit_file","arguments":"{\"path\":\"src/lib.rs\"}"}}]}}]}),
    ];
    let mut wire = String::new();
    for f in &frames {
        wire.push_str(&format!("data: {f}\n\n"));
    }
    wire.push_str("data: [DONE]\n\n");

    let mut s_whole = SessionState::default();
    let whole = feed_chunked_sse(wire.as_bytes(), &[wire.len()], &mut s_whole);

    let mut s_one = SessionState::default();
    let one_byte = feed_chunked_sse(wire.as_bytes(), &[1], &mut s_one);

    let mut s_rand = SessionState::default();
    let random = feed_chunked_sse(wire.as_bytes(), &[1, 3, 2, 7, 1, 5, 13, 2], &mut s_rand);

    assert_eq!(
        whole, one_byte,
        "1-byte SSE fragmentation must be byte-identical"
    );
    assert_eq!(
        whole, random,
        "random SSE fragmentation must be byte-identical"
    );
    assert_eq!(s_whole.last_thought_anchor, s_one.last_thought_anchor);
    assert!(s_one
        .last_thought_anchor
        .as_deref()
        .unwrap_or("")
        .contains("Verified exact fix"));
}

#[test]
fn patch_block_reconstruction_and_treesitter_validation() {
    let src = "fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
    let mut session = SessionState::default();
    session.known_blocks.insert(
        "add#1234".to_owned(),
        BlockSpan {
            block_id: "add#1234".to_owned(),
            file_hint: "lib.rs".to_owned(),
            language: CodeLang::Rust,
            byte_start: 0,
            byte_end: src.len(),
            original_source: src.to_owned(),
        },
    );

    let patch_payload = "@patch:@block:add#1234\n```rust\nfn add(a: i32, b: i32) -> i32 {\n    a.saturating_add(b)\n}\n```";
    let wire = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"delta":{"content": patch_payload}}]})
    );

    let out = feed_chunked_sse(wire.as_bytes(), &[1], &mut session);
    let out_str = String::from_utf8(out).expect("valid utf8");
    assert!(out_str.contains("saturating_add"));
}

#[test]
fn invariant_1_prefix_immutability_and_cache_control_guard() {
    let mut state = SessionState::default();
    let t0 = serde_json::to_vec(&json!({
        "model": "gpt-4o",
        "system": "System instructions. cwd: /workspace/aphelion",
        "messages": [{"role": "user", "content": "Hello"}]
    }))
    .unwrap();
    let out0 =
        aphelion_core::prepare_request(&t0, ExecutionProfile::Ephemeral, &mut state).unwrap();
    let frozen = state.frozen_prefix_len;
    assert!(frozen > 0);

    let t1 = serde_json::to_vec(&json!({
        "model": "gpt-4o",
        "system": "System instructions. cwd: /workspace/aphelion",
        "messages": [
            {"role": "user", "content": "Hello"},
            {"role": "assistant", "content": "Hi there!"},
            {"role": "user", "content": "Follow-up question"}
        ]
    }))
    .unwrap();
    let out1 =
        aphelion_core::prepare_request(&t1, ExecutionProfile::Ephemeral, &mut state).unwrap();
    assert!(verify_prefix_invariant(&out0.body, &out1.body, frozen));
    assert!(state.turn_alias.is_some());

    // Explicit Anthropic cache_control breakpoint must remain 100% untouched.
    let mut anth_state = SessionState::default();
    let anth_t0 = serde_json::to_vec(&json!({
        "anthropic_version": "2023-06-01",
        "max_tokens": 1024,
        "system": [{"type": "text", "text": "Static prompt", "cache_control": {"type": "ephemeral"}}],
        "messages": [{"role": "user", "content": "Hello"}]
    }))
    .unwrap();
    let anth_out =
        aphelion_core::prepare_request(&anth_t0, ExecutionProfile::Ephemeral, &mut anth_state)
            .unwrap();
    assert!(anth_state.sacred_cache_boundary > 0);
    assert!(verify_prefix_invariant(
        &anth_t0,
        &anth_out.body,
        anth_state.frozen_prefix_len
    ));
}

#[test]
fn invariant_2_tool_loop_preserves_tools_and_suppresses_prefill() {
    let mut state = SessionState::default();
    let tools = json!([
        {"type": "function", "function": {"name": "edit_file", "description": "Edit a file"}},
        {"type": "function", "function": {"name": "read_file", "description": "Read a file"}}
    ]);
    let req = serde_json::to_vec(&json!({
        "model": "gpt-4o",
        "tools": tools,
        "messages": [{"role": "user", "content": "Read file src/main.rs"}]
    }))
    .unwrap();
    let out =
        aphelion_core::prepare_request(&req, ExecutionProfile::Ephemeral, &mut state).unwrap();
    let parsed: Value = serde_json::from_slice(&out.body).unwrap();
    assert_eq!(parsed["tools"], tools);
    assert!(!out.metrics.prefill_injected);
    assert!(out.metrics.active_tool_loop);
    let last_content = parsed["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(last_content.contains("[Active Tool Scope:"));
}

#[test]
fn invariant_3_and_5_diet_watermark_syntax_mask_and_tron() {
    // Syntax mask keeps code fences 0% mutated.
    let input = "Please basically note the function:\n```rust\nlet x = the_value + 1;\n```\n";
    let masked = compress_with_syntax_mask(input, ExecutionProfile::Ephemeral);
    assert!(masked.contains("let x = the_value + 1;"));

    // TRON round-trip on homogeneous array >= 3 objects.
    let arr = json!([
        {"id": 1, "name": "alpha", "ok": true},
        {"id": 2, "name": "beta|pipe", "ok": false},
        {"id": 3, "name": "gamma", "ok": true}
    ]);
    let tron = try_encode_tron_table(&arr.to_string()).expect("tron encode");
    assert!(tron.starts_with("@tron["));
    assert_eq!(decode_tron_table(&tron).expect("tron decode"), arr);

    // Diet GC watermark check.
    assert!(estimate_calibrated_tokens("small tool output") < CACHE_AMORTIZATION_WATERMARK_TOKENS);
}

#[test]
fn invariant_6_error_and_circuit_breaker_force_deep_and_disable_prefill() {
    let mut state = SessionState::default();
    let err_req = serde_json::to_vec(&json!({
        "model": "gpt-4o",
        "messages": [
            {"role": "user", "content": "Run tests"},
            {"role": "assistant", "content": "Running"},
            {"role": "tool", "content": "Error: panicked at src/lib.rs:10: assertion failed"}
        ]
    }))
    .unwrap();
    let out1 =
        aphelion_core::prepare_request(&err_req, ExecutionProfile::Ephemeral, &mut state).unwrap();
    assert!(out1.metrics.complexity_deep);
    assert!(!out1.metrics.prefill_injected);
    assert!(!out1.metrics.stage_stats.circuit_breaker_tripped);

    let out2 =
        aphelion_core::prepare_request(&err_req, ExecutionProfile::Ephemeral, &mut state).unwrap();
    assert!(out2.metrics.complexity_deep);
    assert!(out2.metrics.stage_stats.circuit_breaker_tripped);
}

#[test]
fn ephemeral_latency_budget_50kb_payload() {
    let mut state = SessionState::default();
    let big_prose =
        "This is a realistic coding agent context paragraph with identifiers. ".repeat(750);
    let payload = serde_json::to_vec(&json!({
        "model": "gpt-4o",
        "system": "You are a helpful coding assistant.",
        "messages": [{"role": "user", "content": big_prose}]
    }))
    .unwrap();
    assert!(payload.len() >= 50_000);
    let start = std::time::Instant::now();
    let out =
        aphelion_core::prepare_request(&payload, ExecutionProfile::Ephemeral, &mut state).unwrap();
    let elapsed_ms = start.elapsed().as_millis();
    assert!(!out.body.is_empty());
    // Debug builds are slower than release; verify well under 100ms in debug (<3ms in release).
    assert!(
        elapsed_ms < 100,
        "unexpectedly slow transform: {elapsed_ms}ms"
    );
}
