use crate::headers::{
    apply_proxy_headers, decode_incoming_body, has_bypass_header, parse_request_options,
    UpstreamTargets,
};
use crate::public_guard::{
    extract_tenant_hash, render_health_json, MetricsRegistry, PublicConfig, SlidingWindowLimiter,
};
use crate::router::AutoRouter;
use crate::sse_stream::{
    apply_non_streaming_patch, reject_response, static_resp, stream_sse_response,
};
use crate::telemetry_sink::{finalize_turn, prepare_outbound, BoxBody};
use crate::ui_api::{handle_ui_or_api, is_ui_or_api_path};
use crate::ui_state::{detect_client_tag, RuntimePolicy, TelemetryEvent, TopologyTracker};
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
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;
use tokio::net::TcpListener;
use tokio::sync::broadcast;

pub struct ProxyState {
    pub targets: UpstreamTargets,
    pub public_cfg: PublicConfig,
    pub limiter: SlidingWindowLimiter,
    pub metrics: MetricsRegistry,
    pub client: Client,
    pub session: Mutex<SessionState>,
    pub summary: Mutex<SessionCostSummary>,
    pub audit: Option<Mutex<AuditLedger>>,
    pub policy: RwLock<RuntimePolicy>,
    pub topology: Mutex<TopologyTracker>,
    pub events_tx: broadcast::Sender<TelemetryEvent>,
    pub started_at: Instant,
}

impl ProxyState {
    pub fn new(openai: String, anthropic: String, audit: Option<Mutex<AuditLedger>>) -> Self {
        let t = UpstreamTargets {
            openai,
            anthropic,
            gemini: "https://generativelanguage.googleapis.com".to_owned(),
            ollama: "http://127.0.0.1:11434".to_owned(),
        };
        Self::with_config(t, PublicConfig::default(), audit)
    }
    pub fn with_targets(targets: UpstreamTargets, audit: Option<Mutex<AuditLedger>>) -> Self {
        Self::with_config(targets, PublicConfig::default(), audit)
    }
    pub fn with_config(
        targets: UpstreamTargets,
        public_cfg: PublicConfig,
        audit: Option<Mutex<AuditLedger>>,
    ) -> Self {
        let (events_tx, _) = broadcast::channel(256);
        Self {
            targets,
            public_cfg,
            limiter: SlidingWindowLimiter::default(),
            metrics: MetricsRegistry::default(),
            client: Client::new(),
            session: Mutex::new(SessionState::default()),
            summary: Mutex::new(SessionCostSummary::default()),
            audit,
            policy: RwLock::new(RuntimePolicy::default()),
            topology: Mutex::new(TopologyTracker::default()),
            events_tx,
            started_at: Instant::now(),
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

    if method == hyper::Method::GET && path_and_query == "/health" {
        return Ok(static_resp(
            StatusCode::OK,
            "application/json",
            render_health_json(state.public_cfg.public_mode),
        ));
    }
    if method == hyper::Method::GET && path_and_query == "/metrics" {
        return Ok(static_resp(
            StatusCode::OK,
            "text/plain; version=0.0.4",
            state.metrics.render_prometheus(),
        ));
    }
    if is_ui_or_api_path(&path_and_query) {
        let body = if method == hyper::Method::POST {
            req.into_body().collect().await.map(|c| c.to_bytes()).unwrap_or_default()
        } else {
            Bytes::new()
        };
        return Ok(handle_ui_or_api(&method, &path_and_query, &headers, &body, state));
    }

    let tenant_hash = match extract_tenant_hash(&headers, state.public_cfg.public_mode) {
        Ok(h) => h,
        Err(rej) => return Ok(reject_response(rej, &state)),
    };
    if state.public_cfg.public_mode {
        if let Err(rej) = state.limiter.check(tenant_hash, state.public_cfg.rpm_limit) {
            return Ok(reject_response(rej, &state));
        }
    }

    let raw_bytes = match req.into_body().collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => return Ok(static_resp(StatusCode::BAD_REQUEST, "text/plain", "failed reading body".to_owned())),
    };
    if raw_bytes.len() > state.public_cfg.max_body_bytes {
        return Ok(static_resp(StatusCode::PAYLOAD_TOO_LARGE, "text/plain", "request body exceeds max limit".to_owned()));
    }

    let decoded_body = decode_incoming_body(&headers, &raw_bytes);
    let body_json = serde_json::from_slice::<serde_json::Value>(&decoded_body).ok();
    let route = match AutoRouter::resolve(
        &headers,
        &path_and_query,
        body_json.as_ref(),
        &state.targets,
        state.public_cfg.public_mode,
    ) {
        Ok(r) => r,
        Err(rej) => return Ok(reject_response(rej, &state)),
    };

    let should_bypass = method != hyper::Method::POST
        || decoded_body.is_empty()
        || has_bypass_header(&headers)
        || is_passthrough_path(&path_and_query);

    let mut req_opts = parse_request_options(&headers, tenant_hash, &route.provider_id);
    if let Ok(pol) = state.policy.read() {
        pol.apply_to_request(&mut req_opts, &headers);
    }
    let client_tag = detect_client_tag(&headers);
    let (outbound_body, pending) = if should_bypass {
        (decoded_body, None)
    } else {
        prepare_outbound(&state, &decoded_body, req_opts, &route.provider_id, &client_tag)
    };

    let req_method = reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::POST);
    let body_len = outbound_body.len();
    let upstream_req = apply_proxy_headers(
        state.client.request(req_method, &route.target_url),
        &headers,
        body_len,
    )
    .body(outbound_body);

    let upstream_res = match upstream_req.send().await {
        Ok(r) => r,
        Err(err) => {
            eprintln!("[aphelion] upstream connection error: {err}");
            return Ok(static_resp(StatusCode::BAD_GATEWAY, "text/plain", format!("upstream error: {err}")));
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
        if let Some(turn) = pending {
            let ms = (turn.started_at.elapsed().as_millis() as u64).max(1);
            finalize_turn(&state, turn, None, status.as_u16(), ms, ms);
        }
        eprintln!("[aphelion] upstream HTTP {} on {path_and_query}", status.as_u16());
        return Ok(Response::builder()
            .status(status)
            .header("content-type", content_type)
            .body(BoxBody::new(Full::new(err_bytes)))
            .expect("error response"));
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
            let total_ms = (turn.started_at.elapsed().as_millis() as u64).max(1);
            finalize_turn(&state, turn, usage, status.as_u16(), total_ms, total_ms);
        }
    }
    let len = bytes.len();
    Ok(Response::builder()
        .status(status)
        .header("content-type", content_type)
        .header("content-length", len.to_string())
        .body(BoxBody::new(Full::new(Bytes::from(bytes))))
        .expect("json response"))
}
