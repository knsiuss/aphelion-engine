use aphelion_cli::account_vault::{AccountTier, AccountVault, AuthType, ProviderAccount};
use aphelion_cli::headers::UpstreamTargets;
use aphelion_cli::proxy::{run_proxy_server, ProxyState};
use aphelion_core::sse_bridge::OpenAiToAnthropicSseBridge;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

#[test]
fn openai_to_anthropic_sse_bridge_is_one_byte_fragmentation_safe() {
    let stream = concat!(
        "data: {\"id\":\"chatcmpl-1\",\"model\":\"deepseek-chat\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"plan step\"}}]}\n\n",
        "data: {\"id\":\"chatcmpl-1\",\"model\":\"deepseek-chat\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello \"}}]}\n\n",
        "data: {\"id\":\"chatcmpl-1\",\"model\":\"deepseek-chat\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"Read\",\"arguments\":\"{\\\"path\\\":\\\"a.py\\\"}\"}}]}}]}\n\n",
        "data: {\"id\":\"chatcmpl-1\",\"model\":\"deepseek-chat\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":25}}\n\n",
        "data: [DONE]\n\n"
    );

    let mut whole = OpenAiToAnthropicSseBridge::new("deepseek-chat");
    let mut out_whole = whole.push_chunk(stream.as_bytes());
    out_whole.extend_from_slice(&whole.finish());

    let mut byte_by_byte = OpenAiToAnthropicSseBridge::new("deepseek-chat");
    let mut out_1b = Vec::new();
    for b in stream.as_bytes() {
        out_1b.extend_from_slice(&byte_by_byte.push_chunk(&[*b]));
    }
    out_1b.extend_from_slice(&byte_by_byte.finish());

    assert_eq!(out_whole, out_1b, "1-byte SSE fragmentation must match whole stream");
    let text = String::from_utf8(out_whole).expect("utf8");
    assert!(text.contains("event: message_start"));
    assert!(text.contains("thinking_delta"));
    assert!(text.contains("text_delta"));
    assert!(text.contains("input_json_delta"));
    assert!(text.contains("\"stop_reason\":\"tool_use\""));
    assert!(text.contains("event: message_stop"));
}

#[tokio::test]
async fn standalone_gateway_bridges_anthropic_client_to_openai_account_and_masks_secrets() {
    let seen = Arc::new(Mutex::new(Vec::<(String, String, Value)>::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = listener.local_addr().unwrap();
    let seen_clone = Arc::clone(&seen);

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { break };
            let seen = Arc::clone(&seen_clone);
            tokio::spawn(async move {
                let svc = service_fn(move |req: Request<hyper::body::Incoming>| {
                    let seen = Arc::clone(&seen);
                    async move {
                        let path = req.uri().path().to_owned();
                        let auth = req
                            .headers()
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or_default()
                            .to_owned();
                        let raw = req.into_body().collect().await.unwrap().to_bytes();
                        let body: Value = serde_json::from_slice(&raw).unwrap_or_default();
                        seen.lock().unwrap().push((path, auth, body));

                        let sse = concat!(
                            "data: {\"id\":\"chatcmpl-99\",\"model\":\"deepseek-chat\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Standalone works!\"}}]}\n\n",
                            "data: {\"id\":\"chatcmpl-99\",\"model\":\"deepseek-chat\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":40,\"completion_tokens\":8}}\n\n",
                            "data: [DONE]\n\n"
                        );
                        Ok::<_, Infallible>(
                            Response::builder()
                                .status(StatusCode::OK)
                                .header("content-type", "text/event-stream")
                                .body(Full::new(Bytes::from(sse)))
                                .unwrap(),
                        )
                    }
                });
                let _ = http1::Builder::new().serve_connection(TokioIo::new(stream), svc).await;
            });
        }
    });

    let tmp_vault = std::env::temp_dir().join(format!("aphelion_v40_vault_{}.json", std::process::id()));
    let _ = std::fs::remove_file(&tmp_vault);
    let mock_base = format!("http://{mock_addr}");
    let targets = UpstreamTargets {
        openai: mock_base.clone(),
        anthropic: mock_base.clone(),
        gemini: mock_base.clone(),
        ollama: mock_base.clone(),
    };
    let mut state = ProxyState::with_targets(targets, None);
    state.accounts = AccountVault::load_from_path(tmp_vault.clone());
    let state = Arc::new(state);

    let gw_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gw_base = format!("http://{}", gw_listener.local_addr().unwrap());
    let server = tokio::spawn(run_proxy_server(gw_listener, Arc::clone(&state)));
    let client = reqwest::Client::new();

    // 1. Register an OpenAI-dialect account via POST /api/accounts & verify secret masking on GET.
    let acct = ProviderAccount {
        id: "ds-direct".to_owned(),
        label: "DeepSeek Direct".to_owned(),
        provider: "deepseek".to_owned(),
        base_url: mock_base.clone(),
        dialect: "openai".to_owned(),
        auth_type: AuthType::ApiKey,
        secret: "sk-super-secret-9999".to_owned(),
        refresh_token: None,
        expires_at: None,
        tier: AccountTier::Cheap,
        enabled: true,
        daily_budget_usd: Some(5.0),
    };
    let r_add = client
        .post(format!("{gw_base}/api/accounts"))
        .header("content-type", "application/json")
        .body(serde_json::to_vec(&acct).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(r_add.status(), 200);

    let r_list = client.get(format!("{gw_base}/api/accounts")).send().await.unwrap().text().await.unwrap();
    assert!(r_list.contains("sk-...9999"), "secret must be masked");
    assert!(!r_list.contains("sk-super-secret-9999"), "plaintext secret must never leak");

    // 2. Register combo pointing to that OpenAI account.
    let combo_cfg = json!({
        "combos": {
            "standalone": [
                { "model": "deepseek-chat", "account_id": "ds-direct" }
            ]
        }
    });
    assert_eq!(
        client
            .post(format!("{gw_base}/api/combos"))
            .header("content-type", "application/json")
            .body(combo_cfg.to_string())
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    // 3. Send an Anthropic `/v1/messages` request (like Claude Code) -> bridged transparently to OpenAI!
    let anth_req = json!({
        "model": "combo:standalone",
        "max_tokens": 256,
        "stream": true,
        "system": [{ "type": "text", "text": "You are concise." }],
        "tools": [{ "name": "Read", "description": "Read file", "input_schema": { "type": "object" } }],
        "messages": [{ "role": "user", "content": "Say hello" }]
    });
    let resp = client
        .post(format!("{gw_base}/v1/messages"))
        .header("anthropic-version", "2023-06-01")
        .header("x-api-key", "sk-ant-dummy")
        .header("content-type", "application/json")
        .body(anth_req.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let sse_out = resp.text().await.unwrap();
    assert!(sse_out.contains("event: message_start"));
    assert!(sse_out.contains("Standalone works!"));
    assert!(sse_out.contains("event: message_stop"));

    let captured = seen.lock().unwrap().clone();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].0, "/v1/chat/completions");
    assert_eq!(captured[0].1, "Bearer sk-super-secret-9999");
    assert_eq!(captured[0].2["model"], "deepseek-chat");
    assert!(captured[0].2["tools"][0]["function"]["name"] == "Read");

    server.abort();
    let _ = std::fs::remove_file(&tmp_vault);
}

#[test]
fn ide_activator_backs_up_activates_and_restores_claude_settings_safely() {
    use aphelion_cli::ide_activator::{
        activate_integration_in, deactivate_integration_in, inspect_integrations_in,
    };
    let tmp_home = std::env::temp_dir().join(format!("aphelion_ide_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_home);
    let claude_dir = tmp_home.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let settings = claude_dir.join("settings.json");
    let orig = json!({
        "env": { "ANTHROPIC_BASE_URL": "http://localhost:4000" },
        "enabledPlugins": { "my-plugin": true }
    });
    std::fs::write(&settings, serde_json::to_string_pretty(&orig).unwrap()).unwrap();

    let st0 = inspect_integrations_in(&tmp_home, "http://127.0.0.1:18789");
    assert!(!st0[0].active);

    let st1 = activate_integration_in(&tmp_home, "claude_code", "http://127.0.0.1:18789").unwrap();
    assert!(st1.active);
    assert!(st1.can_restore);
    let updated: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(updated["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:18789/h/claude_code");
    assert_eq!(updated["enabledPlugins"]["my-plugin"], true);

    let st2 = deactivate_integration_in(&tmp_home, "claude_code", "http://127.0.0.1:18789").unwrap();
    assert!(!st2.active);
    let restored: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(restored["env"]["ANTHROPIC_BASE_URL"], "http://localhost:4000");
    assert_eq!(restored["enabledPlugins"]["my-plugin"], true);

    let _ = std::fs::remove_dir_all(&tmp_home);
}
