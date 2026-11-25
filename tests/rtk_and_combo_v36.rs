use aphelion_cli::headers::UpstreamTargets;
use aphelion_cli::proxy::{run_proxy_server, ProxyState};
use aphelion_core::rtk_filters::apply_rtk_filter;
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
fn rtk_filters_compress_git_grep_ls_and_build_while_preserving_errors() {
    // 1. git status
    let git_status = "\
On branch main
Your branch is up to date with 'origin/main'.

Changes not staged for commit:
  (use \"git add <file>...\" to update what will be committed)
  (use \"git restore <file>...\" to discard changes in working directory)
\tmodified:   crates/core/src/lib.rs
\tmodified:   crates/core/src/ingress_opt.rs

Untracked files:
  (use \"git add <file>...\" to include in what will be committed)
\tcrates/core/src/rtk_filters.rs

no changes added to commit (use \"git add\" and/or \"git commit -a\")
";
    let gs = apply_rtk_filter("Bash", Some("git status"), git_status).expect("git status filter");
    assert!(gs.contains("On branch main"));
    assert!(gs.contains("crates/core/src/lib.rs"));
    assert!(!gs.contains("use \"git add"));
    assert!(gs.len() < git_status.len() * 65 / 100);

    // 2. git diff (preserves 100% of + / - changed lines, condenses long unchanged runs)
    let mut diff = String::from(
        "diff --git a/src/app.rs b/src/app.rs\nindex 1234567..89abcde 100644\n--- a/src/app.rs\n+++ b/src/app.rs\n@@ -1,20 +1,20 @@\n",
    );
    for i in 1..=14 {
        diff.push_str(&format!(" let unchanged_{i} = {i};\n"));
    }
    diff.push_str("-let old_val = 1;\n+let new_val = 2;\n");
    let gd = apply_rtk_filter("Bash", Some("git diff"), &diff).expect("git diff filter");
    assert!(gd.contains("-let old_val = 1;"));
    assert!(gd.contains("+let new_val = 2;"));
    assert!(gd.contains("unchanged lines"));
    assert!(!gd.contains("index 1234567..89abcde"));

    // 3. git log
    let git_log = "\
commit 1111111111111111111111111111111111111111
Author: Dev <dev@example.com>
Date:   Wed Nov 25 10:00:00 2026 +0700

    feat: add rtk filters

commit 2222222222222222222222222222222222222222
Author: Dev <dev@example.com>
Date:   Tue Nov 24 09:00:00 2026 +0700

    fix: preserve sliding cache_control
";
    let gl = apply_rtk_filter("Bash", Some("git log -n 2"), git_log).expect("git log filter");
    assert!(gl.contains("1111111"));
    assert!(gl.contains("feat: add rtk filters"));
    assert!(!gl.contains("Author:"));
    assert!(gl.len() < git_log.len() / 2);

    // 4. grep / rg grouping
    let mut grep_raw = String::new();
    for i in 1..=8 {
        grep_raw.push_str(&format!(
            "crates/adapters/cli_wrap/src/proxy_handler_module.rs:{}:    let hit_{i} = true;\n",
            i * 10
        ));
    }
    let gr = apply_rtk_filter("Grep", Some("rg hit_"), &grep_raw).expect("grep filter");
    assert!(gr.contains("crates/adapters/cli_wrap/src/proxy_handler_module.rs:"));
    assert!(gr.contains("  10:    let hit_1 = true;"));
    assert!(gr.len() < grep_raw.len() * 65 / 100);

    // 5. ls -la
    let mut ls_raw = String::from("total 256\n");
    for i in 1..=14 {
        ls_raw.push_str(&format!(
            "-rw-r--r-- 1 quincy staff 4096 Nov 25 10:00 module_file_{i:02}.rs\n"
        ));
    }
    let ls = apply_rtk_filter("Bash", Some("ls -la"), &ls_raw).expect("ls filter");
    assert!(ls.contains("4096 module_file_01.rs"));
    assert!(!ls.contains("-rw-r--r--"));

    // 6. cargo build noise condensed when clean, UNTOUCHED when error exists!
    let mut build_ok = String::new();
    for i in 1..=12 {
        build_ok.push_str(&format!("   Compiling dep_crate_{i} v0.1.0\n"));
    }
    build_ok.push_str("    Finished `dev` profile in 3.21s\n");
    let bo = apply_rtk_filter("Bash", Some("cargo check"), &build_ok).expect("build filter");
    assert!(bo.contains("[Aphelion Build Trim: 12 build progress lines condensed]"));
    assert!(bo.contains("Finished `dev` profile"));

    let mut build_err = build_ok.clone();
    build_err.push_str("error[E0382]: borrow of moved value\n");
    assert!(apply_rtk_filter("Bash", Some("cargo check"), &build_err).is_none());
}

#[tokio::test]
async fn combo_pool_round_robins_keys_and_cascades_across_tiers_on_429() {
    let seen = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = listener.local_addr().unwrap();
    let seen_clone = Arc::clone(&seen);

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
                        let auth = req
                            .headers()
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or_default()
                            .to_owned();
                        let raw = req.into_body().collect().await.unwrap().to_bytes();
                        let val: Value = serde_json::from_slice(&raw).unwrap_or_default();
                        let model = val
                            .get("model")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        seen.lock().unwrap().push((model.clone(), auth.clone()));

                        // Simulate Tier-1 key-A rate limited (429), key-B healthy, Tier-2 healthy.
                        if model == "tier1-model" && auth == "Bearer sk-key-a" {
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
                            "id": "chatcmpl-combo",
                            "model": model,
                            "choices": [{"message": {"role": "assistant", "content": format!("served-by-{model}")}}],
                            "usage": {"prompt_tokens": 12, "completion_tokens": 4}
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
        ollama: mock_base.clone(),
    };
    let state = Arc::new(ProxyState::with_targets(targets, None));
    let gw_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gw_base = format!("http://{}", gw_listener.local_addr().unwrap());
    let server = tokio::spawn(run_proxy_server(gw_listener, Arc::clone(&state)));
    let client = reqwest::Client::new();

    // Register 9Router-style `combo:coding` live via POST /api/combos!
    let combo_cfg = json!({
        "combos": {
            "coding": [
                { "model": "tier1-model", "upstream": mock_base, "keys": ["sk-key-a", "sk-key-b"] },
                { "model": "tier2-fallback", "upstream": mock_base, "keys": ["sk-tier2"] }
            ]
        }
    });
    let res_cfg = client
        .post(format!("{gw_base}/api/combos"))
        .header("content-type", "application/json")
        .body(combo_cfg.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(res_cfg.status(), 200);

    let req_body = serde_json::to_vec(&json!({
        "model": "combo:coding",
        "messages": [{"role": "user", "content": "hi"}]
    }))
    .unwrap();

    // Req 1: starts at key-a -> 429 -> transparently cascades to key-b -> 200!
    let r1 = client
        .post(format!("{gw_base}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(req_body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(r1.status(), 200);
    assert!(r1.text().await.unwrap().contains("served-by-tier1-model"));

    // Req 2: key-a is cooling down -> skips directly to key-b -> 200!
    let r2 = client
        .post(format!("{gw_base}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(req_body)
        .send()
        .await
        .unwrap();
    assert_eq!(r2.status(), 200);

    let log = seen.lock().unwrap().clone();
    assert_eq!(
        log,
        vec![
            ("tier1-model".to_owned(), "Bearer sk-key-a".to_owned()),
            ("tier1-model".to_owned(), "Bearer sk-key-b".to_owned()),
            ("tier1-model".to_owned(), "Bearer sk-key-b".to_owned()),
        ]
    );
    server.abort();
}
