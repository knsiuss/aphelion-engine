use aphelion_cli::proxy::{run_proxy_server, ProxyState};
use aphelion_core::canonicalizer::verify_prefix_invariant;
use aphelion_telemetry::audit::AuditLedger;
use bytes::Bytes;
use flate2::write::GzEncoder;
use flate2::Compression;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::json;
use std::convert::Infallible;
use std::io::Write;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

fn resp_json(status: StatusCode, body: &'static str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body)))
        .unwrap()
}

fn resp_sse(body: &'static str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Full::new(Bytes::from(body)))
        .unwrap()
}

async fn spawn_mock_upstream(captured: Arc<Mutex<Vec<Vec<u8>>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let cap = Arc::clone(&captured);
            tokio::spawn(async move {
                let svc = service_fn(move |req: Request<Incoming>| {
                    let cap = Arc::clone(&cap);
                    async move {
                        let path = req.uri().path().to_owned();
                        let body = req.into_body().collect().await.unwrap().to_bytes();
                        cap.lock().unwrap().push(body.to_vec());
                        if path.contains("rate-limit") {
                            return Ok::<_, Infallible>(resp_json(
                                StatusCode::TOO_MANY_REQUESTS,
                                r#"{"error":"rate_limited"}"#,
                            ));
                        }
                        if path.contains("/v1/models") {
                            return Ok(resp_json(StatusCode::OK, r#"{"data":[{"id":"gpt-4o"}]}"#));
                        }
                        if path.contains("/non-stream") {
                            return Ok(resp_json(
                                StatusCode::OK,
                                r#"{"choices":[{"message":{"role":"assistant","content":"Non-stream ok"}}],"usage":{"prompt_tokens":100,"completion_tokens":12,"prompt_tokens_details":{"cached_tokens":80}}}"#,
                            ));
                        }
                        if path.contains("/v1/messages") {
                            let sse = concat!(
                                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":100,\"cache_read_input_tokens\":400,\"cache_creation_input_tokens\":0}}}\n\n",
                                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Checked AST. Ready to patch.\"}}\n\n",
                                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"lib.rs\\\"}\"}}\n\n",
                                "event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":42}}\n\n",
                                "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
                            );
                            return Ok(resp_sse(sse));
                        }
                        let sse = concat!(
                            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"Verified fix. Applying update.\"}}]}\n\n",
                            "data: {\"choices\":[{\"delta\":{\"content\":\"Done!\"}}]}\n\n",
                            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":300,\"completion_tokens\":30,\"prompt_tokens_details\":{\"cached_tokens\":220},\"completion_tokens_details\":{\"reasoning_tokens\":10}}}\n\n",
                            "data: [DONE]\n\n"
                        );
                        Ok(resp_sse(sse))
                    }
                });
                let _ = http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), svc)
                    .await;
            });
        }
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn e2e_multiturn_prefix_freeze_sse_dialects_gzip_and_429_passthrough() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let mock_base = spawn_mock_upstream(Arc::clone(&captured)).await;

    let audit = AuditLedger::open_in_memory().unwrap();
    let state = Arc::new(ProxyState::new(
        format!("{mock_base}/v1"),
        mock_base.clone(),
        Some(Mutex::new(audit)),
    ));
    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let proxy_state = Arc::clone(&state);
    let server = tokio::spawn(async move { run_proxy_server(proxy_listener, proxy_state).await });

    let client = reqwest::Client::new();
    let base = format!("http://{proxy_addr}");

    // 1. Multi-turn OpenAI/DeepSeek SSE over TCP (Turn-0, Turn-1 gzipped, Turn-2)
    let t0 = serde_json::to_vec(&json!({
        "model": "deepseek-reasoner",
        "system": "System prompt cwd: /repo/aphelion",
        "messages": [{"role": "user", "content": "Hello"}]
    }))
    .unwrap();
    let r0 = client
        .post(format!("{base}/v1/chat/completions"))
        .body(t0)
        .send()
        .await
        .unwrap();
    assert_eq!(r0.status(), 200);
    assert!(r0.text().await.unwrap().contains("Done!"));

    let t1 = serde_json::to_vec(&json!({
        "model": "deepseek-reasoner",
        "system": "System prompt cwd: /repo/aphelion",
        "messages": [
            {"role": "user", "content": "Hello"},
            {"role": "assistant", "content": "Done!"},
            {"role": "user", "content": "Next step"}
        ]
    }))
    .unwrap();
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    gz.write_all(&t1).unwrap();
    let gz_body = gz.finish().unwrap();
    let r1 = client
        .post(format!("{base}/v1/chat/completions"))
        .header("content-encoding", "gzip")
        .body(gz_body)
        .send()
        .await
        .unwrap();
    assert_eq!(r1.status(), 200);
    let _ = r1.text().await.unwrap();

    // Assert S_prefix == 1.00 across captured upstream bodies!
    let frozen_len = state.session.lock().unwrap().frozen_prefix_len;
    assert!(frozen_len > 0);
    {
        let cap = captured.lock().unwrap();
        assert!(verify_prefix_invariant(&cap[0], &cap[1], frozen_len));
    }

    // 2. Anthropic SSE dialect round-trip with thinking_delta + input_json_delta + usage
    let anth_body = serde_json::to_vec(&json!({
        "anthropic_version": "2023-06-01",
        "max_tokens": 512,
        "system": "Anthropic agent",
        "messages": [{"role": "user", "content": "Check lib.rs"}]
    }))
    .unwrap();
    let r_anth = client
        .post(format!("{base}/v1/messages"))
        .body(anth_body)
        .send()
        .await
        .unwrap();
    assert_eq!(r_anth.status(), 200);
    let anth_text = r_anth.text().await.unwrap();
    assert!(anth_text.contains("input_json_delta"));

    // 3. Upstream 429 pass-through without crashing or hanging
    let r_429 = client
        .post(format!("{base}/v1/rate-limit"))
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(r_429.status(), 429);
    assert!(r_429.text().await.unwrap().contains("rate_limited"));

    // 4. Non-streaming JSON + X-Aphelion-Upstream + /v1/models GET passthrough + X-Aphelion-Bypass
    let ns_body = serde_json::to_vec(&json!({
        "model": "gpt-4o",
        "stream": false,
        "messages": [{"role": "user", "content": "Hello non-streaming"}]
    }))
    .unwrap();
    let r_ns = client
        .post(format!("{base}/non-stream"))
        .header("x-aphelion-upstream", &mock_base)
        .body(ns_body)
        .send()
        .await
        .unwrap();
    assert_eq!(r_ns.status(), 200);
    assert!(r_ns.text().await.unwrap().contains("Non-stream ok"));

    let r_models = client
        .get(format!("{base}/v1/models"))
        .send()
        .await
        .unwrap();
    assert_eq!(r_models.status(), 200);
    assert!(r_models.text().await.unwrap().contains("gpt-4o"));

    let raw_bypass = br#"{"raw":"untouched"}"#;
    let r_byp = client
        .post(format!("{base}/non-stream"))
        .header("x-aphelion-bypass", "true")
        .body(raw_bypass.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(r_byp.status(), 200);
    assert_eq!(captured.lock().unwrap().last().unwrap(), raw_bypass);

    // 5. Verify SQLite Flight Recorder persisted real upstream cached_tokens & usage
    let audit_sum = state
        .audit
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .summarize()
        .unwrap();
    // 4 successful turns + 1 upstream 429 error captured with status_code in v3.1
    assert_eq!(audit_sum.total_requests, 5);
    assert!(audit_sum.total_cache_read_tokens >= 920);

    server.abort();
}
