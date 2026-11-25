use aphelion_cli::headers::UpstreamTargets;
use aphelion_cli::proxy::{run_proxy_server, ProxyState};
use aphelion_cli::public_guard::PublicConfig;
use aphelion_cli::router::{derive_provider_label, AutoRouter};
use aphelion_telemetry::audit::AuditLedger;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::header::HeaderValue;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{HeaderMap, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

async fn spawn_echo_upstream(captured: Arc<Mutex<Vec<String>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let cap = Arc::clone(&captured);
            tokio::spawn(async move {
                let svc = service_fn(move |req: Request<hyper::body::Incoming>| {
                    let cap = Arc::clone(&cap);
                    async move {
                        let raw = req.into_body().collect().await.unwrap().to_bytes();
                        cap.lock().unwrap().push(String::from_utf8_lossy(&raw).into_owned());
                        let body = json!({
                            "id": "chatcmpl-v32",
                            "choices": [{"message": {"role": "assistant", "content": "done"}}],
                            "usage": {
                                "prompt_tokens": 1000,
                                "completion_tokens": 120,
                                "prompt_tokens_details": { "cached_tokens": 800 },
                                "completion_tokens_details": { "reasoning_tokens": 20 }
                            }
                        });
                        Ok::<_, Infallible>(
                            Response::builder()
                                .status(StatusCode::OK)
                                .header("content-type", "application/json")
                                .body(Full::new(Bytes::from(serde_json::to_vec(&body).unwrap())))
                                .unwrap(),
                        )
                    }
                });
                let _ = http1::Builder::new().serve_connection(TokioIo::new(stream), svc).await;
            });
        }
    });
    format!("http://{addr}")
}

#[test]
fn dynamic_hostname_labeling_never_degrades_valid_hosts_to_unknown() {
    assert_eq!(derive_provider_label("https://api.moonshot.cn/v1"), "moonshot.cn");
    assert_eq!(derive_provider_label("http://127.0.0.1:8000/v1"), "local:8000");
    assert_eq!(derive_provider_label("https://www.tokenharbor.ai/v1"), "tokenharbor.ai");

    let targets = UpstreamTargets {
        openai: "https://api.openai.com/v1".to_owned(),
        anthropic: "https://api.anthropic.com".to_owned(),
        gemini: "https://generativelanguage.googleapis.com".to_owned(),
        ollama: "http://127.0.0.1:11434".to_owned(),
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-aphelion-upstream",
        HeaderValue::from_static("https://api.moonshot.cn/v1"),
    );
    let r = AutoRouter::resolve(&headers, "/v1/chat/completions", None, &targets, false).unwrap();
    assert_eq!(r.provider_id, "moonshot.cn");
    assert_ne!(r.provider_id, "unknown");

    let devstral = json!({"model": "devstral-small-latest"});
    let r2 = AutoRouter::resolve(&HeaderMap::new(), "/v1/chat/completions", Some(&devstral), &targets, false).unwrap();
    assert_eq!(r2.provider_id, "mistral");
}

#[tokio::test]
async fn local_ui_api_topology_and_runtime_policy_toggle_work_end_to_end() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let mock = spawn_echo_upstream(Arc::clone(&captured)).await;
    let db = AuditLedger::open_in_memory().unwrap();
    let targets = UpstreamTargets {
        openai: mock.clone(),
        anthropic: mock.clone(),
        gemini: mock.clone(),
        ollama: mock.clone(),
    };
    let state = Arc::new(ProxyState::with_targets(targets, Some(Mutex::new(db))));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(run_proxy_server(listener, Arc::clone(&state)));
    let client = reqwest::Client::new();

    // 1. UI HTML & JS assets
    let ui_res = client.get(format!("{base}/ui")).send().await.unwrap();
    assert_eq!(ui_res.status(), 200);
    assert!(ui_res.headers()["content-type"].to_str().unwrap().contains("text/html"));
    assert!(ui_res.text().await.unwrap().contains("APHELION"));

    // 2. Proxy an agentic request with default concise output steering
    let tools = json!([{"type": "function", "function": {"name": "read_file", "description": "Read"}}]);
    let p0 = serde_json::to_vec(&json!({
        "model": "gpt-4o",
        "system": "You are a coding assistant.",
        "tools": tools,
        "messages": [{"role": "user", "content": "Say hi"}]
    }))
    .unwrap();
    let res0 = client
        .post(format!("{base}/v1/chat/completions"))
        .header("content-type", "application/json")
        .header("user-agent", "OpenCode/1.0")
        .body(p0)
        .send()
        .await
        .unwrap();
    assert_eq!(res0.status(), 200);
    assert!(captured.lock().unwrap()[0].contains("[Output Policy:"));

    // 3. Toggle runtime policy to verbose + reasoning_effort=low via POST /api/policy without restart
    let pol_res = client
        .post(format!("{base}/api/policy"))
        .header("content-type", "application/json")
        .body(r#"{"output_mode":"verbose","reasoning_effort":"low"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(pol_res.status(), 200);
    let pol_json: Value = serde_json::from_str(&pol_res.text().await.unwrap()).unwrap();
    assert_eq!(pol_json["output_mode"], "verbose");
    assert_eq!(pol_json["reasoning_effort"], "low");

    // Start a fresh session turn-0 against the same running gateway to verify live policy effect
    *state.session.lock().unwrap() = aphelion_core::types::SessionState::default();
    let p1 = serde_json::to_vec(&json!({
        "model": "o3-mini",
        "system": "Fresh session.",
        "tools": tools,
        "messages": [{"role": "user", "content": "Explain"}]
    }))
    .unwrap();
    let res1 = client
        .post(format!("{base}/v1/chat/completions"))
        .header("content-type", "application/json")
        .header("x-aphelion-upstream", &mock)
        .body(p1)
        .send()
        .await
        .unwrap();
    assert_eq!(res1.status(), 200);
    let cap1 = captured.lock().unwrap()[1].clone();
    assert!(!cap1.contains("[Output Policy:"));
    assert!(cap1.contains(r#""reasoning_effort":"low""#));

    // 4. Verify /api/overview and /api/topology reflect live traffic
    let ov: Value = serde_json::from_str(&client.get(format!("{base}/api/overview")).send().await.unwrap().text().await.unwrap()).unwrap();
    assert_eq!(ov["kpis"]["total_requests"], 2);
    assert!(ov["by_provider_model"].as_array().unwrap().len() >= 2);

    let topo: Value = serde_json::from_str(&client.get(format!("{base}/api/topology")).send().await.unwrap().text().await.unwrap()).unwrap();
    assert_eq!(topo["engine"]["output_mode"], "verbose");
    assert!(topo["clients"].as_array().unwrap().iter().any(|c| c["name"] == "OpenCode"));
    assert!(!topo["upstreams"].as_array().unwrap().is_empty());

    server.abort();
}

#[tokio::test]
async fn public_mode_locks_down_ui_and_admin_token_enforces_401_and_200() {
    let targets = UpstreamTargets {
        openai: "https://api.openai.com/v1".to_owned(),
        anthropic: "https://api.anthropic.com".to_owned(),
        gemini: "https://generativelanguage.googleapis.com".to_owned(),
        ollama: "http://127.0.0.1:11434".to_owned(),
    };
    let client = reqwest::Client::new();

    // Case A: --public WITHOUT --admin-token -> 404 on /ui, /api/overview, /api/policy
    let st_locked = Arc::new(ProxyState::with_config(
        UpstreamTargets {
            openai: targets.openai.clone(),
            anthropic: targets.anthropic.clone(),
            gemini: targets.gemini.clone(),
            ollama: targets.ollama.clone(),
        },
        PublicConfig { public_mode: true, rpm_limit: 60, max_body_bytes: 1024 * 1024, admin_token: None },
        None,
    ));
    let l1 = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let b1 = format!("http://{}", l1.local_addr().unwrap());
    let s1 = tokio::spawn(run_proxy_server(l1, st_locked));
    assert_eq!(client.get(format!("{b1}/ui")).send().await.unwrap().status(), 404);
    assert_eq!(client.get(format!("{b1}/api/overview")).send().await.unwrap().status(), 404);
    assert_eq!(client.post(format!("{b1}/api/policy")).body("{}").send().await.unwrap().status(), 404);
    s1.abort();

    // Case B: --public WITH --admin-token secret123 -> 401 unauthorized, 200 authorized
    let st_token = Arc::new(ProxyState::with_config(
        targets,
        PublicConfig {
            public_mode: true,
            rpm_limit: 60,
            max_body_bytes: 1024 * 1024,
            admin_token: Some("secret123".to_owned()),
        },
        None,
    ));
    let l2 = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let b2 = format!("http://{}", l2.local_addr().unwrap());
    let s2 = tokio::spawn(run_proxy_server(l2, st_token));
    assert_eq!(client.get(format!("{b2}/ui")).send().await.unwrap().status(), 401);
    assert_eq!(client.get(format!("{b2}/api/overview")).send().await.unwrap().status(), 401);
    assert_eq!(client.get(format!("{b2}/ui?token=secret123")).send().await.unwrap().status(), 200);
    assert_eq!(
        client.get(format!("{b2}/api/overview")).header("authorization", "Bearer secret123").send().await.unwrap().status(),
        200
    );
    s2.abort();
}
