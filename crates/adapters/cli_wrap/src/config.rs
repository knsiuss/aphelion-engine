use crate::public_guard::PublicConfig;
use aphelion_core::defaults::{
    DEFAULT_COMPACTION_THRESHOLD_TOKENS, DEFAULT_LOCKED_TAIL_MESSAGES, DEFAULT_SLM_ENDPOINT,
};
use std::env;

pub const DEFAULT_GATEWAY_PORT: u16 = 18787;
pub const DEFAULT_LOCAL_HOST: &str = "127.0.0.1";
pub const DEFAULT_PUBLIC_HOST: &str = "0.0.0.0";
pub const DEFAULT_RPM_LIMIT: u32 = 120;
pub const DEFAULT_MAX_BODY_MB: usize = 10;
pub const DEFAULT_EVENTS_CHANNEL_CAP: usize = 256;
pub const DEFAULT_AUDIT_DB: &str = "aphelion_audit.db";
pub const DEFAULT_TOP_INTERVAL_SECS: u64 = 1;

/// Unified runtime configuration following strict precedence:
/// **CLI Flag > `APHELION_*` Env Var > Centralized Default**.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub audit: bool,
    pub public: bool,
    pub admin_token: Option<String>,
    pub port: u16,
    pub bind: Option<String>,
    pub rpm: u32,
    pub max_body_mb: usize,
    pub events_cap: usize,
    pub upstream: Option<String>,
    pub db_path: String,
    pub compaction_threshold: usize,
    pub tail_messages: usize,
    pub slm_endpoint: String,
    pub cmd_start: usize,
}

impl GatewayConfig {
    pub fn env_port(fallback: u16) -> u16 {
        env::var("APHELION_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(fallback)
    }

    pub fn env_db_path() -> String {
        env::var("APHELION_AUDIT_DB")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_AUDIT_DB.to_owned())
    }

    pub fn env_events_cap() -> usize {
        env::var("APHELION_EVENTS_CAP")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&n| n >= 16)
            .unwrap_or(DEFAULT_EVENTS_CHANNEL_CAP)
    }

    pub fn env_core_overrides() -> (usize, usize, String) {
        let compaction = env::var("APHELION_COMPACTION_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_COMPACTION_THRESHOLD_TOKENS);
        let tail = env::var("APHELION_TAIL_MESSAGES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_LOCKED_TAIL_MESSAGES);
        let slm = env::var("APHELION_SLM_ENDPOINT")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_SLM_ENDPOINT.to_owned());
        (compaction, tail, slm)
    }

    pub fn from_env_and_args(args: &[String], fallback_port: u16) -> Self {
        let (compaction_threshold, tail_messages, slm_endpoint) = Self::env_core_overrides();
        let mut cfg = Self {
            audit: false,
            public: false,
            admin_token: env::var("APHELION_ADMIN_TOKEN")
                .ok()
                .filter(|s| !s.trim().is_empty()),
            port: Self::env_port(fallback_port),
            bind: env::var("APHELION_BIND").ok().filter(|s| !s.trim().is_empty()),
            rpm: env::var("APHELION_RPM")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_RPM_LIMIT),
            max_body_mb: env::var("APHELION_MAX_BODY_MB")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_MAX_BODY_MB),
            events_cap: Self::env_events_cap(),
            upstream: env::var("APHELION_UPSTREAM")
                .ok()
                .filter(|s| !s.trim().is_empty()),
            db_path: Self::env_db_path(),
            compaction_threshold,
            tail_messages,
            slm_endpoint,
            cmd_start: 0,
        };

        while cfg.cmd_start < args.len() {
            match args[cfg.cmd_start].as_str() {
                "--audit" => {
                    cfg.audit = true;
                    cfg.cmd_start += 1;
                }
                "--public" => {
                    cfg.public = true;
                    cfg.cmd_start += 1;
                }
                "--admin-token" if cfg.cmd_start + 1 < args.len() => {
                    cfg.admin_token = Some(args[cfg.cmd_start + 1].clone());
                    cfg.cmd_start += 2;
                }
                "--port" if cfg.cmd_start + 1 < args.len() => {
                    cfg.port = args[cfg.cmd_start + 1].parse().unwrap_or(cfg.port);
                    cfg.cmd_start += 2;
                }
                "--bind" if cfg.cmd_start + 1 < args.len() => {
                    cfg.bind = Some(args[cfg.cmd_start + 1].clone());
                    cfg.cmd_start += 2;
                }
                "--rpm" if cfg.cmd_start + 1 < args.len() => {
                    cfg.rpm = args[cfg.cmd_start + 1].parse().unwrap_or(cfg.rpm);
                    cfg.cmd_start += 2;
                }
                "--max-body-mb" if cfg.cmd_start + 1 < args.len() => {
                    cfg.max_body_mb = args[cfg.cmd_start + 1].parse().unwrap_or(cfg.max_body_mb);
                    cfg.cmd_start += 2;
                }
                "--upstream" if cfg.cmd_start + 1 < args.len() => {
                    cfg.upstream = Some(args[cfg.cmd_start + 1].clone());
                    cfg.cmd_start += 2;
                }
                "--db" if cfg.cmd_start + 1 < args.len() => {
                    cfg.db_path = args[cfg.cmd_start + 1].clone();
                    cfg.cmd_start += 2;
                }
                _ => break,
            }
        }
        cfg
    }

    pub fn bind_addr(&self) -> String {
        if let Some(ref b) = self.bind {
            return b.clone();
        }
        let host = if self.public {
            DEFAULT_PUBLIC_HOST
        } else {
            DEFAULT_LOCAL_HOST
        };
        format!("{host}:{}", self.port)
    }

    pub fn public_config(&self) -> PublicConfig {
        let allow_custom = env::var("APHELION_ALLOW_CUSTOM_UPSTREAM")
            .ok()
            .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
        PublicConfig {
            public_mode: self.public,
            rpm_limit: self.rpm,
            max_body_bytes: self.max_body_mb.saturating_mul(1024 * 1024),
            admin_token: self.admin_token.clone(),
            allow_custom_upstream: allow_custom,
        }
    }
}
