use crate::account_vault::ProviderAccount;
use crate::ide_activator::{activate_integration, deactivate_integration, inspect_integrations};
use crate::proxy::ProxyState;
use crate::sse_stream::static_resp;
use crate::telemetry_sink::BoxBody;
use hyper::{HeaderMap, Method, Response, StatusCode};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct IntegrationAction {
    id: String,
    action: String,
    #[serde(default)]
    proxy_base: Option<String>,
}

pub fn is_accounts_api_path(path: &str) -> bool {
    path == "/api/accounts"
        || path.starts_with("/api/accounts/")
        || path == "/api/integrations"
}

fn infer_proxy_base(headers: &HeaderMap, explicit: Option<&str>) -> String {
    if let Some(b) = explicit.filter(|s| !s.trim().is_empty()) {
        return b.trim_end_matches('/').to_owned();
    }
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("127.0.0.1:18789");
    format!("http://{host}")
}

pub fn handle_accounts_api(
    method: &Method,
    path: &str,
    headers: &HeaderMap,
    body: &[u8],
    state: &ProxyState,
) -> Response<BoxBody> {
    if method != Method::GET {
        if let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) {
            if !origin.contains("127.0.0.1") && !origin.contains("localhost") && !state.public_cfg.public_mode {
                return static_resp(StatusCode::FORBIDDEN, "application/json", r#"{"error":"forbidden"}"#.to_owned());
            }
        }
    }

    match (method, path) {
        (&Method::GET, "/api/accounts") => {
            let items: Vec<_> = state
                .accounts
                .list_redacted()
                .into_iter()
                .map(|a| {
                    let q = state.quotas.snapshot(&a.id, a.daily_budget_usd);
                    json!({ "account": a, "quota": q })
                })
                .collect();
            let out = serde_json::to_string(&json!({ "accounts": items })).unwrap_or_else(|_| "{}".to_owned());
            static_resp(StatusCode::OK, "application/json", out)
        }
        (&Method::POST, "/api/accounts") => {
            let Ok(acct) = serde_json::from_slice::<ProviderAccount>(body) else {
                return static_resp(StatusCode::BAD_REQUEST, "application/json", r#"{"error":"invalid account json"}"#.to_owned());
            };
            match state.accounts.upsert(acct) {
                Ok(redacted) => {
                    let out = serde_json::to_string(&redacted).unwrap_or_else(|_| "{}".to_owned());
                    static_resp(StatusCode::OK, "application/json", out)
                }
                Err(e) => static_resp(StatusCode::BAD_REQUEST, "application/json", json!({ "error": e }).to_string()),
            }
        }
        (&Method::DELETE, p) if p.starts_with("/api/accounts/") => {
            let id = p.trim_start_matches("/api/accounts/");
            match state.accounts.remove(id) {
                Ok(true) => static_resp(StatusCode::OK, "application/json", r#"{"removed":true}"#.to_owned()),
                Ok(false) => static_resp(StatusCode::NOT_FOUND, "application/json", r#"{"error":"not found"}"#.to_owned()),
                Err(e) => static_resp(StatusCode::INTERNAL_SERVER_ERROR, "application/json", json!({ "error": e }).to_string()),
            }
        }
        (&Method::GET, "/api/integrations") => {
            let base = infer_proxy_base(headers, None);
            let list = inspect_integrations(&base);
            let out = serde_json::to_string(&json!({ "proxy_base": base, "integrations": list })).unwrap_or_else(|_| "{}".to_owned());
            static_resp(StatusCode::OK, "application/json", out)
        }
        (&Method::POST, "/api/integrations") => {
            let Ok(req) = serde_json::from_slice::<IntegrationAction>(body) else {
                return static_resp(StatusCode::BAD_REQUEST, "application/json", r#"{"error":"invalid integration json"}"#.to_owned());
            };
            let base = infer_proxy_base(headers, req.proxy_base.as_deref());
            let res = if req.action == "activate" {
                activate_integration(&req.id, &base)
            } else {
                deactivate_integration(&req.id, &base)
            };
            match res {
                Ok(st) => static_resp(StatusCode::OK, "application/json", serde_json::to_string(&st).unwrap_or_else(|_| "{}".to_owned())),
                Err(e) => static_resp(StatusCode::BAD_REQUEST, "application/json", json!({ "error": e }).to_string()),
            }
        }
        _ => static_resp(StatusCode::NOT_FOUND, "application/json", r#"{"error":"not found"}"#.to_owned()),
    }
}
