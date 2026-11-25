use aphelion_cli::headers::UpstreamTargets;
use aphelion_cli::proxy::{run_proxy_server, ProxyState};
use aphelion_core::ingress_trim::{strip_ansi_and_spinners, summarize_passing_test_output};
use aphelion_core::types::{ExecutionProfile, SessionState};
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
fn lossless_noise_trimmer_strips_ansi_and_condenses_passing_only_tests() {
    let ansi_spinner = "\x1b[32mCompiling\x1b[0m crate...\n\r[===   ] 40%\r[======] 100% done!\n";
    let clean = strip_ansi_and_spinners(ansi_spinner);
    assert!(!clean.contains('\x1b'));
    assert!(clean.contains("Compiling crate..."));
    assert!(clean.contains("[======] 100% done!"));
    assert!(!clean.contains("40%"));

    // 40 passing test lines -> condensed cleanly while keeping summary.
    let mut pass_log = String::from("running 40 tests\n");
    for i in 1..=40 {
        pass_log.push_str(&format!("test suite::case_{i:02} ... ok\n"));
    }
    pass_log.push_str("\ntest result: ok. 40 passed; 0 failed; 0 ignored; finished in 0.12s\n");
    let condensed = summarize_passing_test_output(&pass_log).expect("should condense passing tests");
    assert!(condensed.contains("[Aphelion Test Trim:"));
    assert!(condensed.contains("test result: ok. 40 passed; 0 failed"));
    assert!(condensed.len() < pass_log.len() / 2);

    // 1 single failure -> MUST return None (0% mutation, never starve debugging!).
    let mut fail_log = pass_log.clone();
    fail_log.push_str("test suite::case_41 ... FAILED\n");
    assert!(summarize_passing_test_output(&fail_log).is_none());
}

#[test]
fn cache_aware_tier_router_routes_short_tasks_and_keeps_warm_sessions_sticky() {
    // 1. Short stateless chat -> routed to fast_model (`ernie-5-5-flash`).
    let mut s_short = SessionState::default();
    s_short.request_options.routing.route_mode = "safe".to_owned();
    s_short.request_options.routing.fast_model = Some("ernie-5-5-flash".to_owned());
    let short_req = serde_json::to_vec(&json!({
        "model": "claude-sonnet-4-5",
        "messages": [{"role": "user", "content": "Generate a 4-word title for this chat."}]
    }))
    .unwrap();
    let out_short =
        aphelion_core::prepare_request(&short_req, ExecutionProfile::Ephemeral, &mut s_short)
            .unwrap();
    let p_short: Value = serde_json::from_slice(&out_short.body).unwrap();
    assert_eq!(p_short["model"], "ernie-5-5-flash");
    assert_eq!(
        out_short.metrics.stage_stats.routed_fast_model.as_deref(),
        Some("ernie-5-5-flash")
    );

    // 2. Warm cached multi-turn session (`turn_index >= 2`, > 3 msgs) -> stays sticky on primary!
    let mut s_warm = SessionState {
        turn_index: 4,
        ..Default::default()
    };
    s_warm.request_options.routing.route_mode = "safe".to_owned();
    s_warm.request_options.routing.fast_model = Some("ernie-5-5-flash".to_owned());
    let warm_req = serde_json::to_vec(&json!({
        "model": "claude-sonnet-4-5",
        "tools": [{"type": "function", "function": {"name": "read_file"}}],
        "messages": [
            {"role": "user", "content": "Build engine"},
            {"role": "assistant", "content": "Reading"},
            {"role": "user", "content": "Next step"},
            {"role": "assistant", "content": "Editing"},
            {"role": "user", "content": "Verify"}
        ]
    }))
    .unwrap();
    let out_warm =
        aphelion_core::prepare_request(&warm_req, ExecutionProfile::Ephemeral, &mut s_warm)
            .unwrap();
    let p_warm: Value = serde_json::from_slice(&out_warm.body).unwrap();
    assert_eq!(p_warm["model"], "claude-sonnet-4-5");
    assert!(out_warm.metrics.stage_stats.routed_fast_model.is_none());

    // 3. Error / stacktrace -> NEVER routed to fast_model.
    let mut s_err = SessionState::default();
    s_err.request_options.routing.route_mode = "safe".to_owned();
    s_err.request_options.routing.fast_model = Some("ernie-5-5-flash".to_owned());
    let err_req = serde_json::to_vec(&json!({
        "model": "claude-sonnet-4-5",
        "messages": [{"role": "user", "content": "Fix Error: panicked at src/lib.rs:42"}]
    }))
    .unwrap();
    let out_err =
        aphelion_core::prepare_request(&err_req, ExecutionProfile::Ephemeral, &mut s_err).unwrap();
    let p_err: Value = serde_json::from_slice(&out_err.body).unwrap();
    assert_eq!(p_err["model"], "claude-sonnet-4-5");
}

#[test]
fn opt_in_auto_reasoning_off_by_default_and_safe_on_mechanical_turns() {
    // Default OFF -> never injects reasoning_effort.
    let mut s_off = SessionState {
        turn_index: 2,
        ..Default::default()
    };
    let mech_req = serde_json::to_vec(&json!({
        "model": "o3-mini",
        "messages": [
            {"role": "user", "content": "Create file"},
            {"role": "assistant", "content": "Done"},
            {"role": "user", "content": "File created successfully."}
        ]
    }))
    .unwrap();
    let out_off =
        aphelion_core::prepare_request(&mech_req, ExecutionProfile::Ephemeral, &mut s_off).unwrap();
    let p_off: Value = serde_json::from_slice(&out_off.body).unwrap();
    assert!(p_off.get("reasoning_effort").is_none());

    // Opt-in safe -> injects `low` on mechanical confirmation, NOT on errors.
    let mut s_safe = SessionState {
        turn_index: 2,
        ..Default::default()
    };
    s_safe.request_options.routing.auto_reasoning_safe = true;
    let out_safe =
        aphelion_core::prepare_request(&mech_req, ExecutionProfile::Ephemeral, &mut s_safe)
            .unwrap();
    let p_safe: Value = serde_json::from_slice(&out_safe.body).unwrap();
    assert_eq!(p_safe["reasoning_effort"], "low");
}

#[tokio::test]
async fn automatic_429_failover_switches_to_fallback_and_respects_cooldown() {
    let seen_models = Arc::new(Mutex::new(Vec::<String>::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = listener.local_addr().unwrap();
    let seen_clone = Arc::clone(&seen_models);

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let seen = Arc::clone(&seen_clone);
            tokio::spawn(async move {
                let svc = service_fn(move |req: Request<hyper::body::Incoming>| {
                    let seen = Arc::clone(&seen);
                    async move {
                        let raw = req.into_body().collect().await.unwrap().to_bytes();
                        let val: Value = serde_json::from_slice(&raw).unwrap_or_default();
                        let model = val
                            .get("model")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        seen.lock().unwrap().push(model.clone());
                        if model == "primary-429" {
                            return Ok::<_, Infallible>(
                                Response::builder()
                                    .status(StatusCode::TOO_MANY_REQUESTS)
                                    .header("retry-after", "60")
                                    .header("content-type", "application/json")
                                    .body(Full::new(Bytes::from(r#"{"error":"rate_limited"}"#)))
                                    .unwrap(),
                            );
                        }
                        let ok = json!({
                            "id": "chatcmpl-fb",
                            "model": model,
                            "choices": [{"message": {"role": "assistant", "content": "ok-fallback"}}],
                            "usage": {"prompt_tokens": 20, "completion_tokens": 5}
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
                let _ = http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), svc)
                    .await;
            });
        }
    });

    let mock_base = format!("http://{mock_addr}");
    let targets = UpstreamTargets {
        openai: mock_base.clone(),
        anthropic: mock_base.clone(),
        gemini: mock_base.clone(),
        ollama: mock_base,
    };
    let state = Arc::new(ProxyState::with_targets(targets, None));
    let gw_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gw_base = format!("http://{}", gw_listener.local_addr().unwrap());
    let server = tokio::spawn(run_proxy_server(gw_listener, Arc::clone(&state)));
    let client = reqwest::Client::new();

    // Configure fallback chain live via POST /api/policy!
    let pol = client
        .post(format!("{gw_base}/api/policy"))
        .header("content-type", "application/json")
        .body(r#"{"fallback_models":["backup-healthy"]}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(pol.status(), 200);

    let req_body = serde_json::to_vec(&json!({
        "model": "primary-429",
        "messages": [{"role": "user", "content": "hello"}]
    }))
    .unwrap();

    // Request 1: hits primary-429 -> HTTP 429 -> transparent failover to backup-healthy -> HTTP 200!
    let r1 = client
        .post(format!("{gw_base}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(req_body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(r1.status(), 200);
    assert!(r1.text().await.unwrap().contains("ok-fallback"));

    // Request 2: primary-429 is now in cooldown -> skips straight to backup-healthy (no wasted 429)!
    let r2 = client
        .post(format!("{gw_base}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(req_body)
        .send()
        .await
        .unwrap();
    assert_eq!(r2.status(), 200);

    let history = seen_models.lock().unwrap().clone();
    assert_eq!(history, vec!["primary-429", "backup-healthy", "backup-healthy"]);
    server.abort();
}
