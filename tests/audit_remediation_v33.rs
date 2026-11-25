use aphelion_cli::headers::{decode_incoming_body_bounded, UpstreamTargets};
use aphelion_cli::proxy::{run_proxy_server, ProxyState};
use aphelion_cli::router::AutoRouter;
use aphelion_core::prefix_ledger::{FrozenSessionRecord, LedgerKey};
use aphelion_core::types::{ExecutionProfile, SessionState};
use aphelion_core::workload::is_passthrough_path;
use aphelion_telemetry::audit::AuditLedger;
use bytes::Bytes;
use flate2::write::GzEncoder;
use flate2::Compression;
use http_body_util::Full;
use hyper::header::HeaderValue;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{HeaderMap, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::io::Write;
use std::sync::Arc;
use tokio::net::TcpListener;

fn default_targets() -> UpstreamTargets {
    UpstreamTargets {
        openai: "https://api.openai.com/v1".to_owned(),
        anthropic: "https://api.anthropic.com".to_owned(),
        gemini: "https://generativelanguage.googleapis.com".to_owned(),
        ollama: "http://127.0.0.1:11434".to_owned(),
    }
}

#[test]
fn multi_conversation_and_haiku_sidecall_do_not_contaminate_session_prefix() {
    let mut state = SessionState::default();
    let sonnet_req = json!({
        "model": "claude-sonnet-4-5",
        "system": "MAIN_SONNET_SYSTEM_PROMPT",
        "messages": [{"role": "user", "content": "Build the authentication module"}]
    });
    let out_sonnet = aphelion_core::prepare_request(&serde_json::to_vec(&sonnet_req).unwrap(), ExecutionProfile::Ephemeral, &mut state).unwrap();
    let p_sonnet: Value = serde_json::from_slice(&out_sonnet.body).unwrap();
    assert_eq!(p_sonnet["model"], "claude-sonnet-4-5");

    // Claude Code Haiku title-generation side-call on the same gateway must NOT inherit Sonnet's model or system!
    let haiku_req = json!({
        "model": "claude-3-5-haiku-latest",
        "system": "Generate a 3-word title.",
        "messages": [{"role": "user", "content": "Summarize title for conversation"}]
    });
    let out_haiku = aphelion_core::prepare_request(&serde_json::to_vec(&haiku_req).unwrap(), ExecutionProfile::Ephemeral, &mut state).unwrap();
    let p_haiku: Value = serde_json::from_slice(&out_haiku.body).unwrap();
    assert_eq!(p_haiku["model"], "claude-3-5-haiku-latest");
    let sys = p_haiku["system"].as_str().unwrap();
    assert!(sys.starts_with("Generate a 3-word title."));
    assert!(!sys.contains("MAIN_SONNET_SYSTEM_PROMPT"));
}

#[test]
fn history_edit_and_rewind_forks_cleanly_without_overwriting_user_edits() {
    let mut state = SessionState::default();
    let t0 = json!({
        "model": "gpt-4o",
        "messages": [
            {"role": "user", "content": "Task 1: Initial question"},
            {"role": "assistant", "content": "Answer 1"},
            {"role": "user", "content": "Original second turn prompt"}
        ]
    });
    aphelion_core::prepare_request(&serde_json::to_vec(&t0).unwrap(), ExecutionProfile::Ephemeral, &mut state).unwrap();

    // User clicks "Edit & Resend" on turn 2 in Cline/Roo Code:
    let edited = json!({
        "model": "gpt-4o",
        "messages": [
            {"role": "user", "content": "Task 1: Initial question"},
            {"role": "assistant", "content": "Answer 1"},
            {"role": "user", "content": "COMPLETELY_EDITED_SECOND_TURN_PROMPT"}
        ]
    });
    let out_edited = aphelion_core::prepare_request(&serde_json::to_vec(&edited).unwrap(), ExecutionProfile::Ephemeral, &mut state).unwrap();
    let p_edited: Value = serde_json::from_slice(&out_edited.body).unwrap();
    assert_eq!(p_edited["messages"][2]["content"].as_str().unwrap(), "COMPLETELY_EDITED_SECOND_TURN_PROMPT");
}

#[test]
fn codex_responses_api_stateless_replay_preserves_initial_file_read() {
    let mut state = SessionState::default();
    let file_content = format!("File: src/main.rs\n{}", "pub fn run() {}\n".repeat(25));
    let t1 = json!({
        "model": "gpt-4o",
        "instructions": "Codex agent",
        "tools": [{"type": "function", "name": "read_file"}],
        "input": [
            {"type": "message", "role": "user", "content": "Read main.rs"},
            {"type": "function_call", "call_id": "c1", "name": "read_file", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "c1", "output": file_content}
        ]
    });
    let out1 = aphelion_core::prepare_request(&serde_json::to_vec(&t1).unwrap(), ExecutionProfile::Ephemeral, &mut state).unwrap();
    let p1: Value = serde_json::from_slice(&out1.body).unwrap();
    assert_eq!(p1["input"][2]["output"].as_str().unwrap(), file_content);

    // Turn 2: Codex statelessly replays full `input` + a new turn without re-reading main.rs.
    let mut t2 = t1.clone();
    t2["input"].as_array_mut().unwrap().push(json!({"type": "message", "role": "user", "content": "Now explain main.rs"}));
    let out2 = aphelion_core::prepare_request(&serde_json::to_vec(&t2).unwrap(), ExecutionProfile::Ephemeral, &mut state).unwrap();
    let p2: Value = serde_json::from_slice(&out2.body).unwrap();
    assert_eq!(p2["input"][2]["output"].as_str().unwrap(), file_content);
}

#[test]
fn ssrf_path_injection_ollama_at_metadata_and_gzip_bomb_blocked() {
    let targets = default_targets();
    assert!(AutoRouter::resolve(&HeaderMap::new(), "/ollama@169.254.169.254/latest/meta-data/", None, &targets, true).is_err());
    assert!(AutoRouter::resolve(&HeaderMap::new(), "/ollama/api/tags", None, &targets, true).is_err());

    let mut enc = GzEncoder::new(Vec::new(), Compression::best());
    enc.write_all(&vec![b'A'; 128 * 1024]).unwrap();
    let compressed = enc.finish().unwrap();
    assert!(compressed.len() < 4096);
    let mut gz_headers = HeaderMap::new();
    gz_headers.insert("content-encoding", HeaderValue::from_static("gzip"));
    assert!(decode_incoming_body_bounded(&gz_headers, &compressed, 16 * 1024).is_none());
}

#[test]
fn zero_retention_prefix_store_never_writes_prompt_or_file_text_to_sqlite() {
    let db_path = std::env::temp_dir().join("aphelion_v33_zero_retention.db");
    let _ = std::fs::remove_file(&db_path);
    let ledger = AuditLedger::open(db_path.to_str().unwrap()).unwrap();
    let key = LedgerKey {
        tenant_hash: 42,
        session_root_hash: 999,
        provider_id: "anthropic".to_owned(),
        model_id: "claude-sonnet-4-5".to_owned(),
    };
    let mut hashes = HashMap::new();
    hashes.insert("src/secret.rs".to_owned(), 0xDEAD_BEEF);
    let rec = FrozenSessionRecord {
        frozen_prefix_bytes: b"TOP_SECRET_SYSTEM_PROMPT_NEVER_PERSIST".to_vec(),
        frozen_messages: vec![json!({"role": "user", "content": "ULTRA_SENSITIVE_USER_CODE_BLOCK"})],
        frozen_file_hashes: hashes,
        committed_len: 3,
        ..FrozenSessionRecord::default()
    };
    ledger.save_prefix_record(&key, &rec).unwrap();

    let conn = Connection::open(&db_path).unwrap();
    let raw_json: String = conn.query_row("SELECT record_json FROM prefix_ledger LIMIT 1", [], |r| r.get(0)).unwrap();
    assert!(!raw_json.contains("TOP_SECRET_SYSTEM_PROMPT_NEVER_PERSIST"));
    assert!(!raw_json.contains("ULTRA_SENSITIVE_USER_CODE_BLOCK"));
    let loaded = ledger.load_prefix_record(&key).unwrap();
    assert_eq!(loaded.committed_len, 3);
    assert_eq!(loaded.frozen_file_hashes.get("src/secret.rs"), Some(&0xDEAD_BEEF));
    let _ = std::fs::remove_file(&db_path);
}

#[test]
fn sk_key_collision_routes_deepseek_moonshot_qwen_and_openai_messages_0_steered() {
    let targets = default_targets();
    let mut sk_headers = HeaderMap::new();
    sk_headers.insert("authorization", HeaderValue::from_static("Bearer sk-1234567890abcdef"));

    let r_ds = AutoRouter::resolve(&sk_headers, "/v1/chat/completions", Some(&json!({"model": "deepseek-chat"})), &targets, false).unwrap();
    assert_eq!(r_ds.provider_id, "deepseek");
    let r_ms = AutoRouter::resolve(&sk_headers, "/v1/chat/completions", Some(&json!({"model": "kimi-k2-0711-preview"})), &targets, false).unwrap();
    assert_eq!(r_ms.provider_id, "moonshot");
    let r_qw = AutoRouter::resolve(&sk_headers, "/v1/chat/completions", Some(&json!({"model": "qwen-max"})), &targets, false).unwrap();
    assert_eq!(r_qw.provider_id, "dashscope");

    let mut proj_headers = HeaderMap::new();
    proj_headers.insert("authorization", HeaderValue::from_static("Bearer sk-proj-openai123"));
    let r_oa = AutoRouter::resolve(&proj_headers, "/v1/chat/completions", Some(&json!({"model": "gpt-4o"})), &targets, false).unwrap();
    assert_eq!(r_oa.provider_id, "openai");

    let mut state = SessionState::default();
    let oa_req = json!({
        "model": "gpt-4o",
        "messages": [
            {"role": "system", "content": "You are an expert Rust coder."},
            {"role": "user", "content": "Write a function"}
        ]
    });
    let out = aphelion_core::prepare_request(&serde_json::to_vec(&oa_req).unwrap(), ExecutionProfile::Ephemeral, &mut state).unwrap();
    let parsed: Value = serde_json::from_slice(&out.body).unwrap();
    assert!(parsed["messages"][0]["content"].as_str().unwrap().contains("[Output Policy:"));
    assert!(is_passthrough_path("/v1/messages/count_tokens?beta=true"));
}

#[tokio::test]
async fn upstream_ratelimit_and_retry_after_headers_preserved_on_429() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let svc = service_fn(|_req: Request<hyper::body::Incoming>| async move {
                Ok::<_, Infallible>(
                    Response::builder()
                        .status(StatusCode::TOO_MANY_REQUESTS)
                        .header("content-type", "application/json")
                        .header("retry-after", "17")
                        .header("x-ratelimit-remaining-tokens", "0")
                        .body(Full::new(Bytes::from_static(br#"{"error":"rate_limit"}"#)))
                        .unwrap(),
                )
            });
            let _ = http1::Builder::new().serve_connection(TokioIo::new(stream), svc).await;
        }
    });

    let state = Arc::new(ProxyState::new(mock_addr.clone(), mock_addr, None));
    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_base = format!("http://{}", proxy_listener.local_addr().unwrap());
    let server = tokio::spawn(run_proxy_server(proxy_listener, state));

    let res = reqwest::Client::new()
        .post(format!("{proxy_base}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 429);
    assert_eq!(res.headers().get("retry-after").unwrap().to_str().unwrap(), "17");
    assert_eq!(res.headers().get("x-ratelimit-remaining-tokens").unwrap().to_str().unwrap(), "0");
    server.abort();
}
