use aphelion_cli::account_vault::{AccountTier, AccountVault, AuthType, ProviderAccount};
use aphelion_cli::combo_pool::ComboConfig;
use aphelion_cli::headers::UpstreamTargets;
use aphelion_cli::proxy::{run_proxy_server, ProxyState};
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

#[tokio::test]
async fn adaptive_failover_recovers_normal_model_429_via_healthy_vault_account() {
    let seen = Arc::new(Mutex::new(Vec::<(String, String, String)>::new()));
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
                        let model = body.get("model").and_then(Value::as_str).unwrap_or_default().to_owned();
                        seen.lock().unwrap().push((path.clone(), model.clone(), auth));

                        if path == "/v1/messages" {
                            return Ok::<_, Infallible>(
                                Response::builder()
                                    .status(StatusCode::TOO_MANY_REQUESTS)
                                    .header("retry-after", "30")
                                    .header("content-type", "application/json")
                                    .body(Full::new(Bytes::from(r#"{"error":"No deployments available"}"#)))
                                    .unwrap(),
                            );
                        }
                        let ok = json!({
                            "id": "chatcmpl-adaptive",
                            "model": model,
                            "choices": [{"message": {"role": "assistant", "content": "Recovered adaptively!"}, "finish_reason": "stop"}],
                            "usage": {"prompt_tokens": 15, "completion_tokens": 5}
                        });
                        Ok::<_, Infallible>(
                            Response::builder()
                                .status(StatusCode::OK)
                                .header("content-type", "application/json")
                                .body(Full::new(Bytes::from(serde_json::to_vec(&ok).unwrap())))
                                .unwrap(),
                        )
                    }
                });
                let _ = http1::Builder::new().serve_connection(TokioIo::new(stream), svc).await;
            });
        }
    });

    let tmp_vault = std::env::temp_dir().join(format!("aphelion_v38_adaptive_{}.json", std::process::id()));
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
    state.combos.replace(ComboConfig::default());
    let state = Arc::new(state);

    let gw_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gw_base = format!("http://{}", gw_listener.local_addr().unwrap());
    let server = tokio::spawn(run_proxy_server(gw_listener, Arc::clone(&state)));
    let client = reqwest::Client::new();

    // User adds an OpenAI-compat backup account; client sends a NORMAL model name (no combo: prefix!).
    let backup = ProviderAccount {
        id: "ds-backup".to_owned(),
        label: "Backup DeepSeek".to_owned(),
        provider: "deepseek".to_owned(),
        base_url: mock_base.clone(),
        dialect: "openai".to_owned(),
        auth_type: AuthType::ApiKey,
        secret: "sk-backup-ok".to_owned(),
        refresh_token: None,
        expires_at: None,
        tier: AccountTier::Cheap,
        enabled: true,
        daily_budget_usd: None,
    };
    assert_eq!(
        client
            .post(format!("{gw_base}/api/accounts"))
            .header("content-type", "application/json")
            .body(serde_json::to_vec(&backup).unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    let anth_req = json!({
        "model": "mistral-deepseek-1017",
        "max_tokens": 128,
        "messages": [{ "role": "user", "content": "hi" }]
    });
    // Req 1: primary /v1/messages returns 429 -> adaptively bridges to OpenAI backup account -> 200!
    let r1 = client
        .post(format!("{gw_base}/v1/messages"))
        .header("anthropic-version", "2023-06-01")
        .header("x-api-key", "sk-primary")
        .header("content-type", "application/json")
        .body(anth_req.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(r1.status(), 200);
    assert!(r1.text().await.unwrap().contains("Recovered adaptively!"));

    // Req 2: primary slot is in cooldown -> skips directly to OpenAI backup account!
    let r2 = client
        .post(format!("{gw_base}/v1/messages"))
        .header("anthropic-version", "2023-06-01")
        .header("x-api-key", "sk-primary")
        .header("content-type", "application/json")
        .body(anth_req.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(r2.status(), 200);

    let log = seen.lock().unwrap().clone();
    assert_eq!(log.len(), 3);
    assert_eq!(log[0].0, "/v1/messages");
    assert_eq!(log[1].0, "/v1/chat/completions");
    assert_eq!(log[1].2, "Bearer sk-backup-ok");
    assert_eq!(log[2].0, "/v1/chat/completions");

    server.abort();
    let _ = std::fs::remove_file(&tmp_vault);
}
