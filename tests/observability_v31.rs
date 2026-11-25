use aphelion_cli::audit_report::{parse_audit_filter, render_audit_report};
use aphelion_cli::headers::UpstreamTargets;
use aphelion_cli::proxy::{run_proxy_server, ProxyState};
use aphelion_cli::top_view::build_top_frame;
use aphelion_core::flight_recorder::AuditFilter;
use aphelion_telemetry::audit::{AuditLedger, AuditRecord, AuditSummary};
use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use rusqlite::Connection;
use serde_json::json;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

async fn spawn_mock_upstream(cached: usize, uncached: usize, output: usize, reasoning: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let svc = service_fn(move |_req: Request<hyper::body::Incoming>| async move {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    let body = json!({
                        "id": "chatcmpl-v31",
                        "choices": [{"message": {"role": "assistant", "content": "ok"}}],
                        "usage": {
                            "prompt_tokens": cached + uncached,
                            "completion_tokens": output,
                            "prompt_tokens_details": { "cached_tokens": cached },
                            "completion_tokens_details": { "reasoning_tokens": reasoning }
                        }
                    });
                    Ok::<_, Infallible>(
                        Response::builder()
                            .status(StatusCode::OK)
                            .header("content-type", "application/json")
                            .body(Full::new(Bytes::from(serde_json::to_vec(&body).unwrap())))
                            .unwrap(),
                    )
                });
                let _ = http1::Builder::new().serve_connection(TokioIo::new(stream), svc).await;
            });
        }
    });
    format!("http://{addr}")
}

#[test]
fn legacy_v2_sqlite_schema_auto_migrates_without_data_loss() {
    let db_path = std::env::temp_dir().join("aphelion_v31_legacy_migration.db");
    let _ = std::fs::remove_file(&db_path);
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE flight_recorder (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                turn_index INTEGER NOT NULL,
                model TEXT NOT NULL,
                raw_tokens INTEGER NOT NULL,
                opt_tokens INTEGER NOT NULL,
                cache_read_tokens INTEGER NOT NULL,
                c_eff_usd REAL NOT NULL,
                slm_routed INTEGER NOT NULL,
                prompt_text TEXT NOT NULL,
                response_text TEXT NOT NULL
            );
            INSERT INTO flight_recorder (
                session_id, turn_index, model, raw_tokens, opt_tokens,
                cache_read_tokens, c_eff_usd, slm_routed, prompt_text, response_text
            ) VALUES ('legacy-s1', 1, 'gpt-4o', 1200, 900, 300, 0.002, 0, '', '');",
        )
        .unwrap();
    }

    let ledger = AuditLedger::open(db_path.to_str().unwrap()).unwrap();
    let rows = ledger.load_all().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].session_id, "legacy-s1");
    assert_eq!(rows[0].provider, "unknown");
    assert_eq!(rows[0].status_code, 200);

    ledger
        .record(&AuditRecord {
            timestamp_ms: 1_700_000_000_000,
            session_id: "v31-s2".to_owned(),
            turn_index: 2,
            provider: "mistral".to_owned(),
            model: "mistral-large-latest".to_owned(),
            status_code: 200,
            raw_tokens: 2000,
            opt_tokens: 1500,
            uncached_input_tokens: 200,
            cache_read_tokens: 1300,
            output_tokens: 400,
            reasoning_tokens: 120,
            ttft_ms: 42,
            total_ms: 110,
            ..AuditRecord::default()
        })
        .unwrap();
    assert_eq!(ledger.load_all().unwrap().len(), 2);
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn per_provider_capture_grouped_summary_filters_and_prometheus_labels() {
    let db_path = std::env::temp_dir().join("aphelion_v31_observability.db");
    let _ = std::fs::remove_file(&db_path);

    let mistral_mock = spawn_mock_upstream(800, 200, 150, 40).await;
    let openrouter_mock = spawn_mock_upstream(5000, 500, 300, 100).await;

    let ledger = AuditLedger::open(db_path.to_str().unwrap()).unwrap();
    let targets = UpstreamTargets {
        openai: mistral_mock.clone(),
        anthropic: openrouter_mock.clone(),
        gemini: "https://generativelanguage.googleapis.com".to_owned(),
        ollama: "http://127.0.0.1:11434".to_owned(),
    };
    let state = Arc::new(ProxyState::with_targets(targets, Some(Mutex::new(ledger))));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(run_proxy_server(listener, Arc::clone(&state)));

    let client = reqwest::Client::new();
    let r1 = client
        .post(format!("{base}/v1/chat/completions"))
        .header("content-type", "application/json")
        .header("x-aphelion-upstream", &mistral_mock)
        .header("x-aphelion-provider", "mistral")
        .body(serde_json::to_vec(&json!({"model": "codestral-latest", "messages": [{"role": "user", "content": "hi"}]})).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(r1.status(), 200);

    let r2 = client
        .post(format!("{base}/v1/chat/completions"))
        .header("content-type", "application/json")
        .header("x-aphelion-upstream", &openrouter_mock)
        .header("x-aphelion-provider", "openrouter")
        .body(serde_json::to_vec(&json!({"model": "deepseek/deepseek-r1:free", "messages": [{"role": "user", "content": "solve"}]})).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(r2.status(), 200);

    let metrics_body = client
        .get(format!("{base}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(metrics_body.contains(r#"aphelion_requests_total{provider="mistral",model="codestral-latest",status_class="2xx"} 1"#));
    assert!(metrics_body.contains(r#"aphelion_requests_total{provider="openrouter",model="deepseek/deepseek-r1:free",status_class="2xx"} 1"#));
    assert!(metrics_body.contains(r#"aphelion_tokens_total{provider="openrouter",model="deepseek/deepseek-r1:free",type="reasoning"} 100"#));
    assert!(metrics_body.contains(r#"aphelion_ttft_ms_count{provider="mistral",model="codestral-latest"} 1"#));
    assert!(metrics_body.contains("aphelion_rate_limited_total 0"));

    server.abort();

    let db = AuditLedger::open(db_path.to_str().unwrap()).unwrap();
    let all = db.load_all().unwrap();
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|r| r.ttft_ms > 0 && r.status_code == 200));

    let full_report = render_audit_report(&db, &AuditFilter::default()).unwrap();
    assert!(full_report.contains("mistral") && full_report.contains("openrouter"));
    assert!(full_report.contains("codestral-latest") && full_report.contains("deepseek/deepseek-r1:free"));

    let args = vec!["--provider".to_owned(), "mistral".to_owned()];
    let filtered = render_audit_report(&db, &parse_audit_filter(&args)).unwrap();
    assert!(filtered.contains("mistral") && !filtered.contains("openrouter"));

    let mut jsonl = Vec::new();
    let exported = db.export_jsonl(&mut jsonl).unwrap();
    assert_eq!(exported, 2);
    let jsonl_str = String::from_utf8(jsonl).unwrap();
    assert!(jsonl_str.contains(r#""provider":"mistral""#) && jsonl_str.contains(r#""ttft_ms""#));
    let _ = std::fs::remove_file(&db_path);
}

#[test]
fn top_frame_renderer_handles_empty_db_and_long_unicode_models() {
    let empty = build_top_frame(&AuditSummary::default(), &[], &[], 1, false);
    assert!(empty.contains("APHELION LIVE MONITOR"));
    assert!(empty.contains("waiting for requests"));

    let long_model = format!("openrouter/very-long-model-name-{}", "αβγδ".repeat(12));
    let tail: Vec<AuditRecord> = (0..12)
        .map(|i| AuditRecord {
            timestamp_ms: 1_700_000_000_000 + (i as i64) * 1000,
            provider: "openrouter".to_owned(),
            model: long_model.clone(),
            status_code: if i % 3 == 0 { 200 } else if i % 3 == 1 { 429 } else { 502 },
            cache_read_tokens: 18_400,
            uncached_input_tokens: 1_200,
            output_tokens: 650,
            reasoning_tokens: 210,
            ttft_ms: 85,
            net_saved_usd: 0.0123,
            ..AuditRecord::default()
        })
        .collect();
    let frame = build_top_frame(&AuditSummary::default(), &[], &tail, 2, true);
    assert!(frame.contains("Interval: 2s"));
    assert!(frame.contains("\x1b[32m200\x1b[0m"));
    assert!(frame.contains("\x1b[33m429\x1b[0m"));
    assert!(frame.contains("\x1b[31m502\x1b[0m"));
    assert_eq!(frame.lines().filter(|l| l.starts_with('[')).count(), 10);
}
