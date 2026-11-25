use aphelion_cli::headers::UpstreamTargets;
use aphelion_cli::proxy::{run_proxy_server, ProxyState};
use aphelion_cli::public_guard::PublicConfig;
use aphelion_cli::router::AutoRouter;
use aphelion_core::stream_fsm::StreamFsm;
use aphelion_core::types::{CompactMode, ExecutionProfile, RequestOptions, SessionState};
use hyper::header::HeaderValue;
use hyper::HeaderMap;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::net::TcpListener;

fn targets() -> UpstreamTargets {
    UpstreamTargets {
        openai: "https://api.openai.com/v1".to_owned(),
        anthropic: "https://api.anthropic.com".to_owned(),
        gemini: "https://generativelanguage.googleapis.com".to_owned(),
        ollama: "http://127.0.0.1:11434/v1".to_owned(),
    }
}

#[test]
fn auto_router_detects_credentials_and_ollama_models() {
    let t = targets();
    let cases = [
        (
            "sk-or-v1-abc",
            "openrouter",
            "https://openrouter.ai/api/v1/chat/completions",
        ),
        (
            "sk-ant-xyz",
            "anthropic",
            "https://api.anthropic.com/v1/chat/completions",
        ),
        (
            "AIzaSyTest",
            "gemini",
            "https://generativelanguage.googleapis.com/v1/chat/completions",
        ),
        (
            "gsk_groq",
            "groq",
            "https://api.groq.com/openai/v1/chat/completions",
        ),
        ("xai-grok", "xai", "https://api.x.ai/v1/chat/completions"),
        (
            "fw_fireworks",
            "fireworks",
            "https://api.fireworks.ai/inference/v1/chat/completions",
        ),
        (
            "hf_token",
            "huggingface",
            "https://router.huggingface.co/v1/chat/completions",
        ),
        (
            "csk-cerebras",
            "cerebras",
            "https://api.cerebras.ai/v1/chat/completions",
        ),
        (
            "pplx-sonar",
            "perplexity",
            "https://api.perplexity.ai/v1/chat/completions",
        ),
        (
            "tgp_v1_tog",
            "together",
            "https://api.together.xyz/v1/chat/completions",
        ),
    ];
    for (tok, prov, url) in cases {
        let mut h = HeaderMap::new();
        h.insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {tok}")).unwrap(),
        );
        let r = AutoRouter::resolve(&h, "/v1/chat/completions", None, &t, false).unwrap();
        assert_eq!((r.provider_id.as_str(), r.target_url.as_str()), (prov, url));
    }
    let body = json!({"model": "qwen2.5-coder:7b", "messages": []});
    let r = AutoRouter::resolve(
        &HeaderMap::new(),
        "/v1/chat/completions",
        Some(&body),
        &t,
        false,
    )
    .unwrap();
    assert_eq!(r.provider_id, "ollama");
}

#[test]
fn output_steering_10_turn_prefix_invariance_and_sse_loop_breaker() {
    let mut state = SessionState::default();
    let mut history = vec![json!({"role": "user", "content": "Start"})];
    let mut prev: Option<Vec<u8>> = None;
    for turn in 0..10usize {
        if turn > 0 {
            history.push(json!({"role": "assistant", "content": format!("Step {turn} done.")}));
            history.push(json!({"role": "user", "content": format!("Next {turn}")}));
        }
        let raw = serde_json::to_vec(
            &json!({"model": "gpt-4o", "system": "Rust eng.", "messages": history}),
        )
        .unwrap();
        let out =
            aphelion_core::prepare_request(&raw, ExecutionProfile::Ephemeral, &mut state).unwrap();
        let p: Value = serde_json::from_slice(&out.body).unwrap();
        assert!(p["system"].as_str().unwrap().contains("[Output Policy:"));
        if let Some(pw) = &prev {
            assert_eq!(&out.body[..pw.len() - 2], &pw[..pw.len() - 2]);
        }
        prev = Some(out.body);
    }

    let mut fsm = StreamFsm::default();
    let mut s = SessionState::default();
    let blk = "This is a degenerate repeated output block exceeding sixty-four bytes of text! ";
    let sse: String = (0..5)
        .map(|_| {
            format!(
                "data: {}\n\n",
                json!({"choices":[{"delta":{"content": blk}}]})
            )
        })
        .collect();
    let out = String::from_utf8_lossy(&fsm.process_sse_chunk(sse.as_bytes(), &mut s)).into_owned();
    assert!(
        fsm.loop_broken && out.contains("\"finish_reason\":\"stop\"") && out.contains("[DONE]")
    );
}

#[test]
fn amortized_compaction_skips_below_120k_and_rolls_epoch_when_forced() {
    let mut state = SessionState::default();
    let tools = json!([{"type": "function", "function": {"name": "read_file"}}]);
    let mut history = vec![json!({"role": "user", "content": "Turn 0 root"})];
    for i in 1..=5 {
        let cid = format!("c{i}");
        history.push(json!({"role": "assistant", "content": "Read", "tool_calls": [{"id": cid, "function": {"name": "read_file", "arguments": "{}"}}]}));
        history.push(json!({"role": "tool", "tool_call_id": cid, "name": "read_file", "content": format!("File: src/f{i}.rs\nfn f{i}() {{}}")}));
    }
    let req0 = serde_json::to_vec(
        &json!({"model": "gpt-4o", "system": "Sys", "tools": tools, "messages": history}),
    )
    .unwrap();
    let out0 =
        aphelion_core::prepare_request(&req0, ExecutionProfile::Ephemeral, &mut state).unwrap();
    assert!(!out0.metrics.stage_stats.macro_compaction_triggered);

    state.request_options.compact_mode = CompactMode::Force;
    history.push(json!({"role": "user", "content": "Compact"}));
    let req1 = serde_json::to_vec(
        &json!({"model": "gpt-4o", "system": "Sys", "tools": tools, "messages": history}),
    )
    .unwrap();
    let out1 =
        aphelion_core::prepare_request(&req1, ExecutionProfile::Ephemeral, &mut state).unwrap();
    assert!(out1.metrics.stage_stats.macro_compaction_triggered);
    let p1: Value = serde_json::from_slice(&out1.body).unwrap();
    assert!(!out1.metrics.stage_stats.validator_rollback);
    assert!(p1["messages"].as_array().unwrap().len() <= 9);
    assert!(p1["messages"][1]["content"]
        .as_str()
        .unwrap()
        .contains("[APHELION_CHECKPOINT epoch=1]"));

    state.request_options.compact_mode = CompactMode::Auto;
    let mut post = p1["messages"].as_array().unwrap().clone();
    post.push(json!({"role": "assistant", "content": "Next."}));
    let req2 = serde_json::to_vec(
        &json!({"model": "gpt-4o", "system": "Sys", "tools": tools, "messages": post}),
    )
    .unwrap();
    let out2 =
        aphelion_core::prepare_request(&req2, ExecutionProfile::Ephemeral, &mut state).unwrap();
    assert_eq!(
        &out2.body[..out1.body.len() - 2],
        &out1.body[..out1.body.len() - 2]
    );
}

#[tokio::test]
async fn public_guard_enforces_401_ssrf_403_tenant_isolation_and_metrics() {
    let mut shared = SessionState::default();
    let payload = serde_json::to_vec(&json!({"model": "gpt-4o", "system": "Sys", "messages": [{"role": "user", "content": "Hi"}]})).unwrap();
    shared.request_options = RequestOptions {
        tenant_hash: 111,
        provider_id: "openai".to_owned(),
        ..RequestOptions::default()
    };
    aphelion_core::prepare_request(&payload, ExecutionProfile::Ephemeral, &mut shared).unwrap();
    let ka = shared.ledger_key("gpt-4o");
    shared.request_options = RequestOptions {
        tenant_hash: 222,
        provider_id: "openai".to_owned(),
        ..RequestOptions::default()
    };
    aphelion_core::prepare_request(&payload, ExecutionProfile::Ephemeral, &mut shared).unwrap();
    let kb = shared.ledger_key("gpt-4o");
    assert_ne!(ka, kb);

    let state = Arc::new(ProxyState::with_config(
        targets(),
        PublicConfig {
            public_mode: true,
            rpm_limit: 10,
            max_body_bytes: 1024 * 1024,
            ..PublicConfig::default()
        },
        None,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(run_proxy_server(listener, state));
    let client = reqwest::Client::new();

    let h = client.get(format!("{base}/health")).send().await.unwrap();
    assert!(h.status() == 200 && h.text().await.unwrap().contains(r#""version":"3.0","mode":"public""#));
    let m = client.get(format!("{base}/metrics")).send().await.unwrap();
    assert!(m.status() == 200 && m.text().await.unwrap().contains("aphelion_requests_total"));
    assert_eq!(client.post(format!("{base}/v1/chat/completions")).body("{}").send().await.unwrap().status(), 401);
    let ssrf = client.post(format!("{base}/v1/chat/completions")).header("authorization", "Bearer sk-tenant-a").header("x-aphelion-upstream", "http://169.254.169.254/latest/meta-data/").body("{}").send().await.unwrap();
    assert_eq!(ssrf.status(), 403);
    server.abort();
}
