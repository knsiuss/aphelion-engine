use crate::headers::{
    apply_proxy_headers, decode_incoming_body, has_bypass_header, resolve_upstream_url,
    UpstreamTargets,
};
use crate::telemetry_sink::{
    apply_non_streaming_patch, finalize_turn, prepare_outbound, stream_sse_response, BoxBody,
};
use aphelion_core::sse_dialect::extract_usage_from_json;
use aphelion_core::types::SessionState;
use aphelion_core::workload::is_passthrough_path;
use aphelion_telemetry::audit::AuditLedger;
use aphelion_telemetry::ledger::SessionCostSummary;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use reqwest::Client;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

pub struct ProxyState {
    pub targets: UpstreamTargets,
    pub client: Client,
    pub session: Mutex<SessionState>,
    pub summary: Mutex<SessionCostSummary>,
    pub audit: Option<Mutex<AuditLedger>>,
}

impl ProxyState {
    pub fn new(
        upstream_openai: String,
        upstream_anthropic: String,
        audit: Option<Mutex<AuditLedger>>,
    ) -> Self {
        Self::with_targets(
            UpstreamTargets {
                openai: upstream_openai,
                anthropic: upstream_anthropic,
                gemini: "https://generativelanguage.googleapis.com".to_owned(),
                ollama: "http://127.0.0.1:11434".to_owned(),
            },
            audit,
        )
    }

    pub fn with_targets(targets: UpstreamTargets, audit: Option<Mutex<AuditLedger>>) -> Self {
        Self {
            targets,
            client: Client::new(),
            session: Mutex::new(SessionState::default()),
            summary: Mutex::new(SessionCostSummary::default()),
            audit,
        }
    }

    pub fn summary_snapshot(&self) -> SessionCostSummary {
        self.summary.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

pub async fn run_proxy_server(listener: TcpListener, state: Arc<ProxyState>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let io = TokioIo::new(stream);
        let st = Arc::clone(&state);
        tokio::spawn(async move {
            let service = service_fn(move |req| handle_request(req, Arc::clone(&st)));
            let _ = http1::Builder::new().serve_connection(io, service).await;
        });
    }
}

async fn handle_request(
    req: Request<Incoming>,
    state: Arc<ProxyState>,
) -> Result<Response<BoxBody>, Infallible> {
    let method = req.method().clone();
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str().to_owned())
        .unwrap_or_else(|| "/".to_owned());
    let headers = req.headers().clone();

    let raw_bytes = match req.into_body().collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => return Ok(error_response(StatusCode::BAD_REQUEST, "failed reading body")),
    };
    let decoded_body = decode_incoming_body(&headers, &raw_bytes);
    let target_url = resolve_upstream_url(&state.targets, &headers, &path_and_query);

    let should_bypass = method != hyper::Method::POST
        || decoded_body.is_empty()
        || has_bypass_header(&headers)
        || is_passthrough_path(&path_and_query);

    let (outbound_body, pending) = if should_bypass {
        (decoded_body, None)
    } else {
        prepare_outbound(&state, &decoded_body)
    };

    let req_method =
        reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::POST);
    let body_len = outbound_body.len();
    let upstream_req = apply_proxy_headers(
        state.client.request(req_method, &target_url),
        &headers,
        body_len,
    )
    .body(outbound_body);

    let upstream_res = match upstream_req.send().await {
        Ok(r) => r,
        Err(err) => {
            eprintln!("[aphelion] upstream connection error: {err}");
            return Ok(error_response(
                StatusCode::BAD_GATEWAY,
                &format!("upstream error: {err}"),
            ));
        }
    };

    let status = StatusCode::from_u16(upstream_res.status().as_u16()).unwrap_or(StatusCode::OK);
    let content_type = upstream_res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_owned();

    if !status.is_success() {
        let err_bytes = upstream_res.bytes().await.unwrap_or_default();
        eprintln!("[aphelion] upstream HTTP {} on {path_and_query}", status.as_u16());
        let resp = Response::builder()
            .status(status)
            .header("content-type", content_type)
            .body(BoxBody::new(Full::new(err_bytes)))
            .unwrap_or_else(|_| error_response(status, "upstream error"));
        return Ok(resp);
    }

    if content_type.contains("text/event-stream") || content_type.contains("application/x-ndjson") {
        return Ok(stream_sse_response(status, content_type, upstream_res, state, pending));
    }

    let mut bytes = upstream_res.bytes().await.unwrap_or_default().to_vec();
    if let Some(turn) = pending {
        if let Ok(mut json) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            let usage = extract_usage_from_json(&json);
            if apply_non_streaming_patch(&mut json, &state) {
                if let Ok(updated) = serde_json::to_vec(&json) {
                    bytes = updated;
                }
            }
            let total_ms = turn.started_at.elapsed().as_millis() as u64;
            finalize_turn(
                &state,
                turn,
                usage,
                total_ms,
                total_ms,
                String::from_utf8_lossy(&bytes).into_owned(),
            );
        }
    }
    let len = bytes.len();
    let resp = Response::builder()
        .status(status)
        .header("content-type", content_type)
        .header("content-length", len.to_string())
        .body(BoxBody::new(Full::new(Bytes::from(bytes))))
        .unwrap_or_else(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "resp build err"));
    Ok(resp)
}

fn error_response(status: StatusCode, msg: &str) -> Response<BoxBody> {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain")
        .body(BoxBody::new(Full::new(Bytes::from(msg.to_owned()))))
        .expect("static response builder")
}
