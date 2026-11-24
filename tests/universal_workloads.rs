use aphelion_core::stream_fsm::StreamFsm;
use aphelion_core::types::{Dialect, ExecutionProfile, SessionState, WorkloadClass};
use aphelion_core::usage_extract::extract_usage_universal;
use serde_json::{json, Value};

#[test]
fn single_turn_rag_compresses_prose_masks_code_and_injects_zero_agentic_directives() {
    let mut state = SessionState::default();
    let req = serde_json::to_vec(&json!({
        "model": "gpt-4o",
        "system": "You are a helpful documentation Q&A assistant.",
        "messages": [{
            "role": "user",
            "content": "Please kindly note that we basically just want to understand the architecture.\n```rust\nlet keep_exact = the_value + 1;\n```\n"
        }]
    }))
    .unwrap();

    let out = aphelion_core::prepare_request(&req, ExecutionProfile::Ephemeral, &mut state).unwrap();
    assert_eq!(out.metrics.dialect, Dialect::OpenAiChat);
    assert_eq!(out.metrics.workload, WorkloadClass::SingleTurnRagOrChat);
    assert!(!out.metrics.prefill_injected);

    let parsed: Value = serde_json::from_slice(&out.body).unwrap();
    let content = parsed["messages"][0]["content"].as_str().unwrap();
    assert!(content.contains("let keep_exact = the_value + 1;"));
    assert!(!content.contains("[Active Tool Scope:"));
    assert!(!content.contains("@patch:@block"));
    assert_eq!(parsed["messages"].as_array().unwrap().len(), 1);
}

#[test]
fn structured_output_openai_and_gemini_disable_prefill_and_tail_directives() {
    let mut s1 = SessionState::default();
    let openai_json = serde_json::to_vec(&json!({
        "model": "gpt-4o",
        "response_format": { "type": "json_schema", "json_schema": { "name": "user", "schema": {"type": "object"} } },
        "messages": [{"role": "user", "content": "Please basically extract the user name from this text."}]
    }))
    .unwrap();
    let o1 = aphelion_core::prepare_request(&openai_json, ExecutionProfile::Ephemeral, &mut s1).unwrap();
    assert_eq!(o1.metrics.workload, WorkloadClass::StructuredOutput);
    assert!(!o1.metrics.prefill_injected);
    let p1: Value = serde_json::from_slice(&o1.body).unwrap();
    assert_eq!(p1["messages"].as_array().unwrap().len(), 1);

    let mut s2 = SessionState::default();
    let gemini_json = serde_json::to_vec(&json!({
        "systemInstruction": {"parts": [{"text": "Extract JSON"}]},
        "generationConfig": {"responseMimeType": "application/json"},
        "contents": [{"role": "user", "parts": [{"text": "Please kindly extract the fields from this record."}]}]
    }))
    .unwrap();
    let o2 = aphelion_core::prepare_request(&gemini_json, ExecutionProfile::Ephemeral, &mut s2).unwrap();
    assert_eq!(o2.metrics.dialect, Dialect::GeminiGenerate);
    assert_eq!(o2.metrics.workload, WorkloadClass::StructuredOutput);
    assert!(!o2.metrics.prefill_injected);
}

#[test]
fn openai_responses_api_tron_and_prefix_freeze() {
    let mut state = SessionState::default();
    let arr = json!([
        {"id": 1, "file": "a.rs", "ok": true},
        {"id": 2, "file": "b.rs", "ok": true},
        {"id": 3, "file": "c.rs", "ok": false}
    ])
    .to_string();
    let req = serde_json::to_vec(&json!({
        "model": "gpt-4.1",
        "instructions": "You are a coding assistant.",
        "previous_response_id": "resp_123",
        "tools": [{"type": "function", "name": "search"}],
        "input": [
            {"type": "function_call_output", "call_id": "call_1", "output": arr}
        ]
    }))
    .unwrap();
    let out = aphelion_core::prepare_request(&req, ExecutionProfile::Ephemeral, &mut state).unwrap();
    assert_eq!(out.metrics.dialect, Dialect::OpenAiResponses);
    let parsed: Value = serde_json::from_slice(&out.body).unwrap();
    assert_eq!(parsed["previous_response_id"], "resp_123");
    assert!(parsed["input"][0]["output"].as_str().unwrap().starts_with("@tron["));
}

#[test]
fn anthropic_array_system_and_thinking_signature_preserved() {
    let mut state = SessionState::default();
    let req = serde_json::to_vec(&json!({
        "anthropic_version": "2023-06-01",
        "max_tokens": 1024,
        "system": [{"type": "text", "text": "Static sys", "cache_control": {"type": "ephemeral"}}],
        "messages": [
            {"role": "user", "content": "Solve this"},
            {"role": "assistant", "content": [{"type": "thinking", "thinking": "abc", "signature": "sig_xyz_999"}, {"type": "text", "text": "ok"}]},
            {"role": "user", "content": "Thanks"}
        ]
    }))
    .unwrap();
    let out = aphelion_core::prepare_request(&req, ExecutionProfile::Ephemeral, &mut state).unwrap();
    let parsed: Value = serde_json::from_slice(&out.body).unwrap();
    assert_eq!(parsed["messages"][1]["content"][0]["signature"], "sig_xyz_999");
}

#[test]
fn gemini_usage_metadata_and_ollama_ndjson_streaming() {
    let gem = json!({"usageMetadata": {"promptTokenCount": 400, "candidatesTokenCount": 50, "cachedContentTokenCount": 300}});
    let u = extract_usage_universal(&gem).unwrap();
    assert_eq!(u.cached_tokens, 300);
    assert_eq!(u.input_tokens, 400);

    let mut fsm = StreamFsm::default();
    let mut session = SessionState::default();
    let ndjson = concat!(
        "{\"model\":\"qwen2.5-coder\",\"message\":{\"role\":\"assistant\",\"content\":\"Hi\"},\"done\":false}\n",
        "{\"model\":\"qwen2.5-coder\",\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\"prompt_eval_count\":120,\"eval_count\":25}\n"
    );
    let out = fsm.process_sse_chunk(ndjson.as_bytes(), &mut session);
    assert_eq!(out, ndjson.as_bytes());
    let ollama_u = session.last_usage.unwrap();
    assert_eq!(ollama_u.input_tokens, 120);
    assert_eq!(ollama_u.output_tokens, 25);
}

#[test]
fn unknown_or_embeddings_payload_passes_through_byte_identically() {
    let mut state = SessionState::default();
    let emb = br#"{"model":"text-embedding-3-small","input":"hello world"}"#;
    let out = aphelion_core::prepare_request(emb, ExecutionProfile::Ephemeral, &mut state).unwrap();
    assert_eq!(out.metrics.dialect, Dialect::Passthrough);
    assert_eq!(out.metrics.workload, WorkloadClass::Passthrough);
    assert_eq!(out.body, emb);
}
