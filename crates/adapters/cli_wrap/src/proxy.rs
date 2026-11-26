use crate::account_vault::AccountVault;
use crate::combo_pool::ComboPool;
use crate::config::GatewayConfig;
use crate::failover::{dispatch_with_failover, CooldownTracker};
use crate::quota_tracker::QuotaTracker;
use crate::headers::{
    decode_incoming_body_bounded, extract_model, has_bypass_header, parse_request_options,
    UpstreamTargets,
};
use crate::provider_registry::{DEFAULT_GEMINI_BASE, DEFAULT_OLLAMA_BASE};
use crate::public_guard::{
    extract_tenant_hash, redact_path_query, render_health_json, MetricsRegistry, PublicConfig,
    SlidingWindowLimiter,
};
use crate::router::AutoRouter;
use crate::sse_stream::{
    apply_non_streaming_patch, copy_upstream_headers, reject_response, static_resp,
    stream_sse_response,
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
use std::time::{Duration, Instant};
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
    pub accounts: AccountVault,
    pub quotas: QuotaTracker,
    pub combos: ComboPool,
    pub failover: CooldownTracker,
    pub topology: Mutex<TopologyTracker>,
    pub events_tx: broadcast::Sender<TelemetryEvent>,
    pub started_at: Instant,
}

impl ProxyState {
    pub fn new(openai: String, anthropic: String, audit: Option<Mutex<AuditLedger>>) -> Self {
        let t = UpstreamTargets {
            openai,
            anthropic,
            gemini: DEFAULT_GEMINI_BASE.to_owned(),
            ollama: DEFAULT_OLLAMA_BASE.to_owned(),
        };
        Self::with_config(t, PublicConfig::default(), audit)
    }
    pub fn with_targets(targets: UpstreamTargets, audit: Option<Mutex<AuditLedger>>) -> Self {
        Self::with_config(targets, PublicConfig::default(), audit)
    }
    pub fn with_config(targets: UpstreamTargets, public_cfg: PublicConfig, audit: Option<Mutex<AuditLedger>>) -> Self {
        let (events_tx, _) = broadcast::channel(GatewayConfig::env_events_cap());
        let client = Client::builder().redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15)).build().unwrap_or_else(|_| Client::new());
        Self {
            targets, public_cfg, limiter: SlidingWindowLimiter::default(), metrics: MetricsRegistry::default(),
            client, session: Mutex::new(SessionState::default()), summary: Mutex::new(SessionCostSummary::default()),
            audit, policy: RwLock::new(RuntimePolicy::default()), accounts: AccountVault::default(),
            quotas: QuotaTracker::default(), combos: ComboPool::default(), failover: CooldownTracker::default(),
            topology: Mutex::new(TopologyTracker::default()), events_tx, started_at: Instant::now(),
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
        return Ok(static_resp(StatusCode::OK, "application/json", render_health_json(state.public_cfg.public_mode)));
    }
    if method == hyper::Method::GET && path_and_query == "/metrics" {
        return Ok(static_resp(StatusCode::OK, "text/plain; version=0.0.4", state.metrics.render_prometheus()));
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

    let Ok(collected) = req.into_body().collect().await else {
        return Ok(static_resp(StatusCode::BAD_REQUEST, "text/plain", "failed reading body".to_owned()));
    };
    let raw_bytes = collected.to_bytes();
    let Some(decoded_body) = decode_incoming_body_bounded(&headers, &raw_bytes, state.public_cfg.max_body_bytes) else {
        return Ok(static_resp(StatusCode::PAYLOAD_TOO_LARGE, "text/plain", "request body exceeds max limit".to_owned()));
    };

    let (harness_id, eff_path) = crate::harness_io::strip_harness_prefix(&path_and_query)
        .map(|(h, p)| (Some(h), p)).unwrap_or_else(|| (None, path_and_query.clone()));
    let body_json = serde_json::from_slice::<serde_json::Value>(&decoded_body).ok();
    let learned = harness_id.as_deref().and_then(|hid| {
        let rp = crate::harness_io::default_harness_routes_path(&crate::spawn::user_home_dir());
        crate::harness_io::load_harness_routes_from(&rp).remove(hid)
    });
    let route = if let (Some(hid), Some(base)) = (&harness_id, learned) {
        if state.public_cfg.public_mode {
            if let Err(rej) = crate::public_guard::validate_upstream_ssrf(&base) { return Ok(reject_response(rej, &state)); }
        }
        crate::router::ResolvedRoute { provider_id: hid.clone(), target_url: crate::headers::join_base_path(&base, &eff_path) }
    } else {
        match AutoRouter::resolve(&headers, &eff_path, body_json.as_ref(), &state.targets, state.public_cfg.public_mode) {
            Ok(r) => r, Err(rej) => return Ok(reject_response(rej, &state)),
        }
    };

    let should_bypass = method != hyper::Method::POST || decoded_body.is_empty()
        || has_bypass_header(&headers) || is_passthrough_path(&eff_path);
    let mut req_opts = parse_request_options(&headers, tenant_hash, &route.provider_id);
    let (fallbacks, adaptive_fallback) = state.policy.read()
        .map(|pol| { pol.apply_to_request(&mut req_opts, &headers); (pol.fallback_models.clone(), pol.adaptive_fallback) })
        .unwrap_or((Vec::new(), true));
    let client_tag = harness_id.unwrap_or_else(|| detect_client_tag(&headers));
    let (outbound_body, mut pending) = if should_bypass { (decoded_body, None) }
        else { prepare_outbound(&state, &decoded_body, req_opts, &route.provider_id, &client_tag) };

    let req_method = reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::POST);
    let slots = if should_bypass {
        Vec::new()
    } else {
        let sid = pending.as_ref().map(|p| p.session_id.as_str());
        let escalate = pending.as_ref().is_some_and(|p| p.escalate_to_frontier);
        state.combos.resolve_slots_adaptive(
            &extract_model(&outbound_body), &fallbacks, sid, escalate, Some(&state.accounts), Some(&state.quotas), adaptive_fallback,
        )
    };
    let Some(disp) = dispatch_with_failover(
        &state.client, req_method, &route.target_url, &eff_path, &headers, outbound_body, &slots, &state.failover, &state.quotas,
    ).await else {
        eprintln!("[aphelion] upstream connection error on {}", redact_path_query(&eff_path));
        return Ok(static_resp(StatusCode::BAD_GATEWAY, "text/plain", "upstream connection failed".to_owned()));
    };
    let (upstream_res, bridged) = (disp.response, disp.bridged_oai_to_anthropic);
    if let Some(ref mut turn) = pending {
        if !disp.model.is_empty() { turn.model = disp.model.clone(); }
    }

    let status = StatusCode::from_u16(upstream_res.status().as_u16()).unwrap_or(StatusCode::OK);
    let upstream_headers = upstream_res.headers().clone();
    let content_type = upstream_headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("application/json").to_owned();

    if !status.is_success() {
        let err_bytes = upstream_res.bytes().await.unwrap_or_default();
        if let Some(turn) = pending {
            let ms = (turn.started_at.elapsed().as_millis() as u64).max(1);
            finalize_turn(&state, turn, None, status.as_u16(), ms, ms);
        }
        eprintln!("[aphelion] upstream HTTP {} on {}", status.as_u16(), redact_path_query(&path_and_query));
        return Ok(copy_upstream_headers(Response::builder().status(status).header("content-type", content_type), &upstream_headers)
            .body(BoxBody::new(Full::new(err_bytes)))
            .expect("error response"));
    }

    if content_type.contains("text/event-stream") || content_type.contains("application/x-ndjson") {
        return Ok(stream_sse_response(status, content_type, upstream_res, state, pending, bridged));
    }

    let mut bytes = upstream_res.bytes().await.unwrap_or_default().to_vec();
    if bridged {
        let fallback_m = pending.as_ref().map(|p| p.model.as_str()).unwrap_or_default();
        if let Ok(anth) = aphelion_core::dialect_bridge::translate_openai_to_anthropic_response(&bytes, fallback_m) {
            bytes = anth;
        }
    }
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
    let builder = Response::builder().status(status).header("content-type", content_type).header("content-length", len.to_string());
    Ok(copy_upstream_headers(builder, &upstream_headers).body(BoxBody::new(Full::new(Bytes::from(bytes)))).expect("json response"))
}
