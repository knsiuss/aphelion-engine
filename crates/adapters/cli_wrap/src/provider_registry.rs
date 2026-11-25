use crate::headers::UpstreamTargets;
use std::env;

/// Data-driven upstream provider descriptor (single source of truth).
/// Every entry can be overridden via `APHELION_UPSTREAM_<PROVIDER>`.
pub struct ProviderSpec {
    pub id: &'static str,
    pub path_prefix: &'static str,
    pub default_base: &'static str,
    pub key_prefixes: &'static [&'static str],
    pub model_prefixes: &'static [&'static str],
}

pub const DEFAULT_OPENAI_BASE: &str = "https://api.openai.com/v1";
pub const DEFAULT_ANTHROPIC_BASE: &str = "https://api.anthropic.com";
pub const DEFAULT_GEMINI_BASE: &str = "https://generativelanguage.googleapis.com";
pub const DEFAULT_OLLAMA_BASE: &str = "http://127.0.0.1:11434";

pub const PROVIDER_SPECS: &[ProviderSpec] = &[
    ProviderSpec {
        id: "openrouter",
        path_prefix: "/openrouter",
        default_base: "https://openrouter.ai/api/v1",
        key_prefixes: &["sk-or-v1-"],
        model_prefixes: &["openrouter/", "stealth/"],
    },
    ProviderSpec {
        id: "anthropic",
        path_prefix: "/anthropic",
        default_base: DEFAULT_ANTHROPIC_BASE,
        key_prefixes: &["sk-ant-"],
        model_prefixes: &["claude-"],
    },
    ProviderSpec {
        id: "gemini",
        path_prefix: "/gemini",
        default_base: DEFAULT_GEMINI_BASE,
        key_prefixes: &["AIza"],
        model_prefixes: &["gemini-"],
    },
    ProviderSpec {
        id: "groq",
        path_prefix: "/groq",
        default_base: "https://api.groq.com/openai/v1",
        key_prefixes: &["gsk_"],
        model_prefixes: &[],
    },
    ProviderSpec {
        id: "xai",
        path_prefix: "/xai",
        default_base: "https://api.x.ai/v1",
        key_prefixes: &["xai-"],
        model_prefixes: &["grok-"],
    },
    ProviderSpec {
        id: "fireworks",
        path_prefix: "/fireworks",
        default_base: "https://api.fireworks.ai/inference/v1",
        key_prefixes: &["fw_"],
        model_prefixes: &[],
    },
    ProviderSpec {
        id: "huggingface",
        path_prefix: "/huggingface",
        default_base: "https://router.huggingface.co/v1",
        key_prefixes: &["hf_"],
        model_prefixes: &[],
    },
    ProviderSpec {
        id: "cerebras",
        path_prefix: "/cerebras",
        default_base: "https://api.cerebras.ai/v1",
        key_prefixes: &["csk-"],
        model_prefixes: &[],
    },
    ProviderSpec {
        id: "perplexity",
        path_prefix: "/perplexity",
        default_base: "https://api.perplexity.ai",
        key_prefixes: &["pplx-"],
        model_prefixes: &["sonar-"],
    },
    ProviderSpec {
        id: "together",
        path_prefix: "/together",
        default_base: "https://api.together.xyz/v1",
        key_prefixes: &["tgp_v1_"],
        model_prefixes: &[],
    },
    ProviderSpec {
        id: "github",
        path_prefix: "/github",
        default_base: "https://models.github.ai/inference",
        key_prefixes: &["ghp_", "github_pat_"],
        model_prefixes: &[],
    },
    ProviderSpec {
        id: "mistral",
        path_prefix: "/mistral",
        default_base: "https://api.mistral.ai/v1",
        key_prefixes: &[],
        model_prefixes: &["mistral-", "devstral-", "codestral-"],
    },
    ProviderSpec {
        id: "deepseek",
        path_prefix: "/deepseek",
        default_base: "https://api.deepseek.com/v1",
        key_prefixes: &[],
        model_prefixes: &["deepseek-"],
    },
    ProviderSpec {
        id: "moonshot",
        path_prefix: "/moonshot",
        default_base: "https://api.moonshot.ai/v1",
        key_prefixes: &[],
        model_prefixes: &["kimi-", "moonshot-"],
    },
    ProviderSpec {
        id: "dashscope",
        path_prefix: "/dashscope",
        default_base: "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
        key_prefixes: &[],
        model_prefixes: &["qwen-", "qwen2", "qwen3", "qwq-"],
    },
    ProviderSpec {
        id: "zhipu",
        path_prefix: "/zhipu",
        default_base: "https://open.bigmodel.cn/api/paas/v4",
        key_prefixes: &[],
        model_prefixes: &["glm-"],
    },
    ProviderSpec {
        id: "siliconflow",
        path_prefix: "/siliconflow",
        default_base: "https://api.siliconflow.cn/v1",
        key_prefixes: &[],
        model_prefixes: &[],
    },
    ProviderSpec {
        id: "opencode",
        path_prefix: "/opencode",
        default_base: DEFAULT_OPENAI_BASE,
        key_prefixes: &[],
        model_prefixes: &[],
    },
    ProviderSpec {
        id: "ollama",
        path_prefix: "/ollama",
        default_base: DEFAULT_OLLAMA_BASE,
        key_prefixes: &[],
        model_prefixes: &[],
    },
    ProviderSpec {
        id: "openai",
        path_prefix: "/openai",
        default_base: DEFAULT_OPENAI_BASE,
        key_prefixes: &["sk-proj-", "sk-svcacct-"],
        model_prefixes: &["gpt-", "o1-", "o3-", "o4-"],
    },
];

pub fn resolve_provider_base(spec: &ProviderSpec, targets: &UpstreamTargets) -> String {
    let env_key = format!("APHELION_UPSTREAM_{}", spec.id.to_ascii_uppercase());
    if let Ok(custom) = env::var(&env_key) {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return trimmed.to_owned();
        }
    }
    match spec.id {
        "openai" | "opencode" => targets.openai.clone(),
        "anthropic" => targets.anthropic.clone(),
        "gemini" => targets.gemini.clone(),
        "ollama" => targets.ollama.clone(),
        _ => spec.default_base.to_owned(),
    }
}

pub fn build_upstream_targets(override_url: Option<&str>) -> UpstreamTargets {
    let openai = override_url
        .map(str::to_owned)
        .or_else(|| env::var("APHELION_UPSTREAM_OPENAI").ok())
        .or_else(|| env::var("OPENAI_BASE_URL").ok())
        .unwrap_or_else(|| DEFAULT_OPENAI_BASE.to_owned());
    let anthropic = override_url
        .map(str::to_owned)
        .or_else(|| env::var("APHELION_UPSTREAM_ANTHROPIC").ok())
        .or_else(|| env::var("ANTHROPIC_BASE_URL").ok())
        .unwrap_or_else(|| DEFAULT_ANTHROPIC_BASE.to_owned());
    let gemini = env::var("APHELION_UPSTREAM_GEMINI")
        .or_else(|_| env::var("GEMINI_API_BASE_URL"))
        .or_else(|_| env::var("GOOGLE_GEMINI_BASE_URL"))
        .unwrap_or_else(|_| DEFAULT_GEMINI_BASE.to_owned());
    let ollama = env::var("APHELION_UPSTREAM_OLLAMA")
        .or_else(|_| env::var("OLLAMA_HOST"))
        .unwrap_or_else(|_| DEFAULT_OLLAMA_BASE.to_owned());
    UpstreamTargets {
        openai,
        anthropic,
        gemini,
        ollama,
    }
}
