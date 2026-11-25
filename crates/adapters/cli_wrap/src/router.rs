use crate::headers::{join_base_path, UpstreamTargets};
use crate::provider_registry::{resolve_provider_base, DEFAULT_OPENAI_BASE, PROVIDER_SPECS};
use crate::public_guard::{
    extract_raw_credential, ssrf_reject, validate_upstream_ssrf, GuardRejection,
};
use hyper::HeaderMap;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRoute {
    pub provider_id: String,
    pub target_url: String,
}

/// Deterministic 4-Stage Zero-Config Auto-Router backed by `PROVIDER_SPECS`:
/// 1. Explicit `X-Aphelion-Upstream` (with SSRF guard) or strict `/provider/...` path prefix.
/// 2. Unambiguous credential prefix (`sk-or-v1-`, `sk-ant-`, `sk-proj-`, `AIza`, `gsk_`, etc.).
/// 3. Wire-dialect (`/v1/messages`, `anthropic-version`) & Model-ID disambiguation (resolving generic `sk-` collisions for DeepSeek, Moonshot, Qwen, GLM, Mistral, LiteLLM).
/// 4. Generic `sk-` / dialect fallback + dynamic hostname label derivation.
pub struct AutoRouter;

impl AutoRouter {
    pub fn resolve(
        headers: &HeaderMap,
        path_and_query: &str,
        body_json: Option<&Value>,
        targets: &UpstreamTargets,
        public_mode: bool,
    ) -> Result<ResolvedRoute, GuardRejection> {
        // Stage 1a: Explicit `X-Aphelion-Upstream` header override
        if let Some(override_base) = headers
            .get("x-aphelion-upstream")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if public_mode {
                validate_upstream_ssrf(override_base)?;
            }
            let prov = headers
                .get("x-aphelion-provider")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| derive_provider_label(override_base));
            return Ok(ResolvedRoute {
                provider_id: prov,
                target_url: join_base_path(override_base, path_and_query),
            });
        }

        // Reject `@` authority injection attempts in path_and_query (e.g., `/ollama@169.254...`).
        let path_only = path_and_query.split('?').next().unwrap_or(path_and_query);
        if path_only.contains('@') {
            return Err(ssrf_reject("Invalid `@` authority character in request path"));
        }

        // Stage 1b: Explicit provider path prefix (must be followed by `/`, `?`, or end-of-path)
        for spec in PROVIDER_SPECS {
            if let Some(rest) = path_and_query.strip_prefix(spec.path_prefix) {
                if rest.is_empty() || rest.starts_with('/') || rest.starts_with('?') {
                    if public_mode && spec.id == "ollama" {
                        return Err(ssrf_reject("Local Ollama route is disabled in --public mode"));
                    }
                    let base = resolve_provider_base(spec, targets);
                    return Ok(ResolvedRoute {
                        provider_id: spec.id.to_owned(),
                        target_url: join_base_path(&base, rest),
                    });
                }
            }
        }

        // Stage 2: Unambiguous credential-shape auto-detection (in-memory only, never logged)
        let raw_token = extract_raw_credential(headers);
        if let Some(token) = raw_token {
            for spec in PROVIDER_SPECS {
                if spec.key_prefixes.iter().any(|p| token.starts_with(p)) {
                    let base = resolve_provider_base(spec, targets);
                    return Ok(ResolvedRoute {
                        provider_id: spec.id.to_owned(),
                        target_url: join_base_path(&base, path_and_query),
                    });
                }
            }
        }

        // Stage 3a: Anthropic wire-dialect priority (handles LiteLLM `sk-...` virtual keys on `/v1/messages` & `/api/hello`)
        if path_only.contains("/messages") || headers.contains_key("anthropic-version") {
            return Ok(ResolvedRoute {
                provider_id: "anthropic".to_owned(),
                target_url: join_base_path(&targets.anthropic, path_and_query),
            });
        }
        if path_only.contains("generateContent") {
            return Ok(ResolvedRoute {
                provider_id: "gemini".to_owned(),
                target_url: join_base_path(&targets.gemini, path_and_query),
            });
        }
        if is_ollama_api_path(path_only) {
            if public_mode {
                return Err(ssrf_reject("Local Ollama route is disabled in --public mode"));
            }
            return Ok(ResolvedRoute {
                provider_id: "ollama".to_owned(),
                target_url: join_base_path(&targets.ollama, path_and_query),
            });
        }

        // Stage 3b: Model-ID disambiguation (disambiguates generic `sk-` keys across DeepSeek, Moonshot, Qwen, GLM, Mistral, Ollama)
        if targets.openai.starts_with(DEFAULT_OPENAI_BASE) {
            let model = body_json
                .and_then(|v| v.get("model"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if let Some((provider, base)) = detect_provider_by_model(model, targets) {
                return Ok(ResolvedRoute {
                    provider_id: provider.to_owned(),
                    target_url: join_base_path(&base, path_and_query),
                });
            }
        }

        // Stage 4: Generic `sk-` / OpenAI-compatible fallback
        let label = if targets.openai.starts_with(DEFAULT_OPENAI_BASE) {
            "openai".to_owned()
        } else {
            derive_provider_label(&targets.openai)
        };
        Ok(ResolvedRoute {
            provider_id: label,
            target_url: join_base_path(&targets.openai, path_and_query),
        })
    }
}

fn is_ollama_api_path(path: &str) -> bool {
    matches!(
        path,
        "/api/chat"
            | "/api/generate"
            | "/api/tags"
            | "/api/embed"
            | "/api/embeddings"
            | "/api/show"
            | "/api/ps"
            | "/api/pull"
            | "/api/version"
    )
}

fn detect_provider_by_model(model: &str, targets: &UpstreamTargets) -> Option<(&'static str, String)> {
    let m = model.trim().to_ascii_lowercase();
    if m.is_empty() {
        return None;
    }
    if m.ends_with(":free") {
        let spec = PROVIDER_SPECS.iter().find(|s| s.id == "openrouter")?;
        return Some((spec.id, resolve_provider_base(spec, targets)));
    }
    if m.contains(':') {
        let spec = PROVIDER_SPECS.iter().find(|s| s.id == "ollama")?;
        return Some((spec.id, resolve_provider_base(spec, targets)));
    }
    for spec in PROVIDER_SPECS {
        if spec.model_prefixes.iter().any(|p| m.starts_with(p)) {
            return Some((spec.id, resolve_provider_base(spec, targets)));
        }
    }
    None
}

/// Derives a clean, dynamic provider label from any custom URL:
/// e.g., `https://api.moonshot.cn/v1` -> `"moonshot.cn"`,
/// `http://127.0.0.1:8000/v1` -> `"local:8000"`.
pub fn derive_provider_label(url_str: &str) -> String {
    let Ok(parsed) = reqwest::Url::parse(url_str.trim()) else {
        return "custom".to_owned();
    };
    let Some(host) = parsed.host_str() else {
        return "custom".to_owned();
    };
    let lower = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    if matches!(lower.as_str(), "127.0.0.1" | "localhost" | "::1" | "0.0.0.0") {
        return match parsed.port() {
            Some(p) => format!("local:{p}"),
            None => "local".to_owned(),
        };
    }
    let clean = lower
        .strip_prefix("api.")
        .or_else(|| lower.strip_prefix("www."))
        .unwrap_or(&lower);
    if clean.is_empty() {
        "custom".to_owned()
    } else {
        clean.to_owned()
    }
}
