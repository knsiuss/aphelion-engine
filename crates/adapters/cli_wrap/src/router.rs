use crate::headers::{join_base_path, UpstreamTargets};
use crate::public_guard::{extract_raw_credential, validate_upstream_ssrf, GuardRejection};
use hyper::HeaderMap;
use serde_json::Value;
use std::env;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRoute {
    pub provider_id: String,
    pub target_url: String,
}

/// Deterministic 4-Stage Zero-Config Auto-Router:
/// 1. Explicit `X-Aphelion-Upstream` (with SSRF guard in `--public` mode) or path prefix.
/// 2. Credential-shape auto-detection (in-memory only, never logged).
/// 3. Model-ID pattern auto-detection (fallback for generic/local keys).
/// 4. Wire-dialect path fallback.
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
            return Ok(ResolvedRoute {
                provider_id: "custom".to_owned(),
                target_url: join_base_path(override_base, path_and_query),
            });
        }

        // Stage 1b: Explicit provider path prefix
        if let Some((provider, base, rest)) = match_path_prefix(path_and_query, targets) {
            return Ok(ResolvedRoute {
                provider_id: provider.to_owned(),
                target_url: join_base_path(base, rest),
            });
        }

        // Stage 2: Credential-shape auto-detection (in-memory only, never logged)
        if let Some(token) = extract_raw_credential(headers) {
            if let Some((provider, base)) = detect_provider_by_credential(token, targets) {
                return Ok(ResolvedRoute {
                    provider_id: provider.to_owned(),
                    target_url: join_base_path(base, path_and_query),
                });
            }
        }

        // Stage 3: Model-ID pattern auto-detection (only when not pinned to a custom router/mock)
        let is_default_openai = targets.openai.starts_with("https://api.openai.com");
        if is_default_openai {
            let model = body_json
                .and_then(|v| v.get("model"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if let Some((provider, base)) = detect_provider_by_model(model, targets) {
                return Ok(ResolvedRoute {
                    provider_id: provider.to_owned(),
                    target_url: join_base_path(base, path_and_query),
                });
            }
        }

        // Stage 4: Dialect path fallback
        let (provider, base) = if path_and_query.contains("/messages") {
            ("anthropic", targets.anthropic.as_str())
        } else if path_and_query.contains("generateContent") {
            ("gemini", targets.gemini.as_str())
        } else if path_and_query.starts_with("/api/") {
            ("ollama", targets.ollama.as_str())
        } else {
            ("openai", targets.openai.as_str())
        };
        Ok(ResolvedRoute {
            provider_id: provider.to_owned(),
            target_url: join_base_path(base, path_and_query),
        })
    }
}

fn match_path_prefix<'a>(
    path: &'a str,
    targets: &'a UpstreamTargets,
) -> Option<(&'static str, &'a str, &'a str)> {
    let table: &[(&str, &str, &str)] = &[
        ("/openai", "openai", targets.openai.as_str()),
        ("/anthropic", "anthropic", targets.anthropic.as_str()),
        ("/gemini", "gemini", targets.gemini.as_str()),
        ("/openrouter", "openrouter", "https://openrouter.ai/api/v1"),
        ("/opencode", "opencode", targets.openai.as_str()),
        ("/groq", "groq", "https://api.groq.com/openai/v1"),
        ("/xai", "xai", "https://api.x.ai/v1"),
        (
            "/fireworks",
            "fireworks",
            "https://api.fireworks.ai/inference/v1",
        ),
        (
            "/huggingface",
            "huggingface",
            "https://router.huggingface.co/v1",
        ),
        ("/cerebras", "cerebras", "https://api.cerebras.ai/v1"),
        ("/perplexity", "perplexity", "https://api.perplexity.ai"),
        ("/together", "together", "https://api.together.xyz/v1"),
        ("/deepseek", "deepseek", "https://api.deepseek.com/v1"),
        ("/mistral", "mistral", "https://api.mistral.ai/v1"),
        ("/ollama", "ollama", targets.ollama.as_str()),
    ];
    for &(prefix, provider, base) in table {
        if let Some(rest) = path.strip_prefix(prefix) {
            return Some((provider, base, rest));
        }
    }
    None
}

fn detect_provider_by_credential<'a>(
    token: &str,
    targets: &'a UpstreamTargets,
) -> Option<(&'static str, &'a str)> {
    if token.starts_with("sk-or-v1-") {
        Some(("openrouter", "https://openrouter.ai/api/v1"))
    } else if token.starts_with("sk-ant-") {
        Some(("anthropic", targets.anthropic.as_str()))
    } else if token.starts_with("AIza") {
        Some(("gemini", targets.gemini.as_str()))
    } else if token.starts_with("gsk_") {
        Some(("groq", "https://api.groq.com/openai/v1"))
    } else if token.starts_with("xai-") {
        Some(("xai", "https://api.x.ai/v1"))
    } else if token.starts_with("fw_") {
        Some(("fireworks", "https://api.fireworks.ai/inference/v1"))
    } else if token.starts_with("hf_") {
        Some(("huggingface", "https://router.huggingface.co/v1"))
    } else if token.starts_with("csk-") {
        Some(("cerebras", "https://api.cerebras.ai/v1"))
    } else if token.starts_with("pplx-") {
        Some(("perplexity", "https://api.perplexity.ai"))
    } else if token.starts_with("tgp_v1_") {
        Some(("together", "https://api.together.xyz/v1"))
    } else if token.starts_with("sk-proj-") || token.starts_with("sk-") {
        Some(("openai", targets.openai.as_str()))
    } else {
        None
    }
}

fn detect_provider_by_model<'a>(
    model: &str,
    targets: &'a UpstreamTargets,
) -> Option<(&'static str, &'a str)> {
    let m = model.trim().to_ascii_lowercase();
    if m.is_empty() {
        return None;
    }
    if m.ends_with(":free") || m.starts_with("openrouter/") || m.starts_with("stealth/") {
        Some(("openrouter", "https://openrouter.ai/api/v1"))
    } else if m.starts_with("claude-") {
        Some(("anthropic", targets.anthropic.as_str()))
    } else if m.starts_with("gemini-") {
        Some(("gemini", targets.gemini.as_str()))
    } else if m.starts_with("grok-") {
        Some(("xai", "https://api.x.ai/v1"))
    } else if m.starts_with("sonar-") {
        Some(("perplexity", "https://api.perplexity.ai"))
    } else if m.starts_with("mistral-") || m.starts_with("codestral-") {
        Some(("mistral", "https://api.mistral.ai/v1"))
    } else if m.starts_with("deepseek-") && !m.contains(':') {
        Some(("deepseek", "https://api.deepseek.com/v1"))
    } else if m.contains(':') {
        Some(("ollama", targets.ollama.as_str()))
    } else {
        None
    }
}

/// `aphelion doctor`: detects local IDEs and prints 1-line `baseURL` integration snippets
/// without reading or modifying any user secrets.
pub fn run_doctor() {
    let home = env::var("USERPROFILE")
        .or_else(|_| env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));

    println!("Aphelion v3 Doctor — Zero-Config IDE Onboarding");
    println!("Start gateway: aphelion serve --port 18787 --audit\n");

    let checks = [
        (
            "OpenCode",
            home.join(".config/opencode"),
            r#"Set provider baseURL: "http://127.0.0.1:18787/v1""#,
        ),
        (
            "Cursor",
            home.join(".cursor"),
            "Settings -> Models -> Override OpenAI Base URL: http://127.0.0.1:18787/v1",
        ),
        (
            "Cline",
            home.join(".vscode/extensions"),
            "Cline Settings -> OpenAI Compatible Base URL: http://127.0.0.1:18787/v1",
        ),
        (
            "Continue",
            home.join(".continue"),
            "config.yaml -> apiBase: http://127.0.0.1:18787/v1",
        ),
    ];
    for (name, path, hint) in checks {
        let status = if path.exists() {
            "DETECTED"
        } else {
            "not found"
        };
        println!("  [{status:^9}] {name:<10} -> {hint}");
    }
}
