use crate::proxy::ProxyState;
use crate::public_guard::PublicConfig;
use crate::sse_stream::static_resp;
use crate::telemetry_sink::BoxBody;
use crate::ui_queries::{build_overview_json, build_topology_json};
use crate::ui_state::PolicyPatch;
use bytes::Bytes;
use futures_util::stream;
use http_body_util::{Full, StreamBody};
use hyper::body::Frame;
use hyper::{HeaderMap, Method, Response, StatusCode};
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;

const UI_HTML: &str = include_str!("../ui/index.html");
const UI_CSS: &str = include_str!("../ui/styles.css");
const UI_APP_JS: &str = include_str!("../ui/app.js");
const UI_TOPO_JS: &str = include_str!("../ui/topology.js");
const UI_CHARTS_JS: &str = include_str!("../ui/charts.js");
const UI_CSP: &str = "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:";

pub fn is_ui_or_api_path(path_and_query: &str) -> bool {
    let path = path_and_query.split('?').next().unwrap_or(path_and_query);
    path == "/ui"
        || path.starts_with("/ui/")
        || matches!(
            path,
            "/api/overview" | "/api/topology" | "/api/events" | "/api/policy"
        )
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub fn check_ui_access(
    headers: &HeaderMap,
    path_and_query: &str,
    cfg: &PublicConfig,
) -> Result<(), StatusCode> {
    if !cfg.public_mode {
        return Ok(());
    }
    let Some(ref secret) = cfg.admin_token else {
        return Err(StatusCode::NOT_FOUND);
    };
    if let Some(auth) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        let trimmed = auth.trim();
        if trimmed.len() > 7 && trimmed[..7].eq_ignore_ascii_case("bearer ") {
            let candidate = trimmed[7..].trim();
            if constant_time_eq(candidate.as_bytes(), secret.as_bytes()) {
                return Ok(());
            }
        }
    }
    if let Some(query) = path_and_query.split_once('?').map(|(_, q)| q) {
        for pair in query.split('&') {
            if let Some(tok) = pair.strip_prefix("token=") {
                if constant_time_eq(tok.as_bytes(), secret.as_bytes()) {
                    return Ok(());
                }
            }
        }
    }
    Err(StatusCode::UNAUTHORIZED)
}

pub fn handle_ui_or_api(
    method: &Method,
    path_and_query: &str,
    headers: &HeaderMap,
    body: &[u8],
    state: Arc<ProxyState>,
) -> Response<BoxBody> {
    if let Err(code) = check_ui_access(headers, path_and_query, &state.public_cfg) {
        let msg = if code == StatusCode::NOT_FOUND {
            r#"{"error":"not found"}"#
        } else {
            r#"{"error":"unauthorized"}"#
        };
        return static_resp(code, "application/json", msg.to_owned());
    }

    let path = path_and_query.split('?').next().unwrap_or(path_and_query);
    match (method, path) {
        (&Method::GET, "/ui" | "/ui/") => Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/html; charset=utf-8")
            .header("content-security-policy", UI_CSP)
            .body(BoxBody::new(Full::new(Bytes::from(UI_HTML))))
            .unwrap_or_else(|_| static_resp(StatusCode::OK, "text/html", UI_HTML.to_owned())),
        (&Method::GET, "/ui/styles.css") => {
            static_resp(StatusCode::OK, "text/css; charset=utf-8", UI_CSS.to_owned())
        }
        (&Method::GET, "/ui/app.js") => static_resp(
            StatusCode::OK,
            "application/javascript; charset=utf-8",
            UI_APP_JS.to_owned(),
        ),
        (&Method::GET, "/ui/topology.js") => static_resp(
            StatusCode::OK,
            "application/javascript; charset=utf-8",
            UI_TOPO_JS.to_owned(),
        ),
        (&Method::GET, "/ui/charts.js") => static_resp(
            StatusCode::OK,
            "application/javascript; charset=utf-8",
            UI_CHARTS_JS.to_owned(),
        ),
        (&Method::GET, "/api/overview") => {
            static_resp(StatusCode::OK, "application/json", build_overview_json(&state))
        }
        (&Method::GET, "/api/topology") => {
            static_resp(StatusCode::OK, "application/json", build_topology_json(&state))
        }
        (&Method::GET, "/api/events") => stream_events_sse(state),
        (&Method::POST, "/api/policy") => handle_policy_update(headers, body, &state),
        _ => static_resp(
            StatusCode::NOT_FOUND,
            "application/json",
            r#"{"error":"not found"}"#.to_owned(),
        ),
    }
}

fn handle_policy_update(headers: &HeaderMap, body: &[u8], state: &ProxyState) -> Response<BoxBody> {
    if let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) {
        if !origin.contains("127.0.0.1") && !origin.contains("localhost") && !state.public_cfg.public_mode {
            return static_resp(
                StatusCode::FORBIDDEN,
                "application/json",
                r#"{"error":"cross-origin policy mutation forbidden"}"#.to_owned(),
            );
        }
    }
    let Ok(patch) = serde_json::from_slice::<PolicyPatch>(body) else {
        return static_resp(
            StatusCode::BAD_REQUEST,
            "application/json",
            r#"{"error":"invalid policy json"}"#.to_owned(),
        );
    };
    let updated = if let Ok(mut pol) = state.policy.write() {
        pol.apply_patch(patch);
        pol.clone()
    } else {
        Default::default()
    };
    let json_str = serde_json::to_string(&updated).unwrap_or_else(|_| "{}".to_owned());
    static_resp(StatusCode::OK, "application/json", json_str)
}

fn stream_events_sse(state: Arc<ProxyState>) -> Response<BoxBody> {
    let rx = state.events_tx.subscribe();
    let s = stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let payload = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".to_owned());
                    let frame = format!("event: request\ndata: {payload}\n\n");
                    return Some((Ok::<Frame<Bytes>, Infallible>(Frame::data(Bytes::from(frame))), rx));
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return None,
            }
        }
    });
    let boxed: Pin<Box<dyn futures_util::Stream<Item = Result<Frame<Bytes>, Infallible>> + Send>> =
        Box::pin(s);
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .body(BoxBody::new(StreamBody::new(boxed)))
        .expect("sse events response")
}
