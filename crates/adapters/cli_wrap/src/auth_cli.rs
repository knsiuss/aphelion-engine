use crate::account_vault::{
    default_aphelion_dir, AccountTier, AccountVault, AuthType, ProviderAccount,
};
use crate::combo_pool::{default_combos_path, ComboConfig, ComboStrategy, ComboTarget};
use std::collections::HashMap;
use std::process::ExitCode;

/// `aphelion auth <add|list|remove>` CLI handler for standalone multi-account management.
pub fn run_auth_cmd(args: &[String]) -> ExitCode {
    let vault = AccountVault::default();
    match args.first().map(String::as_str) {
        Some("list") => {
            let list = vault.list_redacted();
            if list.is_empty() {
                println!("No accounts configured in {}", vault.path().display());
                println!("Run `aphelion init` or `aphelion auth add <id> --provider deepseek --url https://api.deepseek.com --key sk-...`");
                return ExitCode::SUCCESS;
            }
            println!("Aphelion Accounts ({}):", vault.path().display());
            for a in list {
                let budget = a
                    .daily_budget_usd
                    .map(|b| format!("${b:.2}/day"))
                    .unwrap_or_else(|| "unlimited".to_owned());
                println!(
                    "  {:<18} provider={:<10} dialect={:<9} tier={:?} key={} budget={} url={}",
                    a.id, a.provider, a.dialect, a.tier, a.secret_masked, budget, a.base_url
                );
            }
            ExitCode::SUCCESS
        }
        Some("add") => run_auth_add(&vault, &args[1..]),
        Some("remove" | "rm" | "delete") => {
            let Some(id) = args.get(1) else {
                eprintln!("Usage: aphelion auth remove <account_id>");
                return ExitCode::from(2);
            };
            match vault.remove(id) {
                Ok(true) => {
                    println!("Removed account `{id}`.");
                    ExitCode::SUCCESS
                }
                Ok(false) => {
                    eprintln!("Account `{id}` not found.");
                    ExitCode::from(1)
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    ExitCode::from(1)
                }
            }
        }
        _ => {
            println!("Usage:\n  aphelion auth list\n  aphelion auth add <id> --provider <name> --url <base_url> --key <secret> [--dialect openai|anthropic] [--tier free|subscription|cheap|paid] [--budget <usd>]\n  aphelion auth remove <id>");
            ExitCode::from(2)
        }
    }
}

fn run_auth_add(vault: &AccountVault, args: &[String]) -> ExitCode {
    let Some(id) = args.first().cloned() else {
        eprintln!("Usage: aphelion auth add <id> --provider <p> --url <url> --key <secret>");
        return ExitCode::from(2);
    };
    let mut provider = "custom".to_owned();
    let mut base_url = String::new();
    let mut secret = String::new();
    let mut dialect = "openai".to_owned();
    let mut tier = AccountTier::Paid;
    let mut budget = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--provider" if i + 1 < args.len() => { provider = args[i + 1].clone(); i += 2; }
            "--url" if i + 1 < args.len() => { base_url = args[i + 1].clone(); i += 2; }
            "--key" if i + 1 < args.len() => { secret = args[i + 1].clone(); i += 2; }
            "--dialect" if i + 1 < args.len() => { dialect = args[i + 1].to_ascii_lowercase(); i += 2; }
            "--budget" if i + 1 < args.len() => { budget = args[i + 1].parse::<f64>().ok(); i += 2; }
            "--tier" if i + 1 < args.len() => {
                tier = match args[i + 1].to_ascii_lowercase().as_str() {
                    "free" => AccountTier::Free,
                    "subscription" | "sub" => AccountTier::Subscription,
                    "cheap" => AccountTier::Cheap,
                    _ => AccountTier::Paid,
                };
                i += 2;
            }
            _ => i += 1,
        }
    }
    let acct = ProviderAccount {
        id: id.clone(),
        label: id,
        provider,
        base_url,
        dialect,
        auth_type: AuthType::ApiKey,
        secret,
        refresh_token: None,
        expires_at: None,
        tier,
        enabled: true,
        daily_budget_usd: budget,
    };
    match vault.upsert(acct) {
        Ok(r) => {
            println!("Saved account `{}` (key={}, dialect={}, url={})", r.id, r.secret_masked, r.dialect, r.base_url);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::from(2)
        }
    }
}

/// `aphelion init`: initializes `~/.aphelion/` with starter accounts and combo templates.
pub fn run_init_cmd() -> ExitCode {
    let dir = default_aphelion_dir();
    let _ = std::fs::create_dir_all(&dir);
    let combos_path = default_combos_path();
    if !combos_path.exists() {
        let mut combos = HashMap::new();
        combos.insert(
            "fast-coding".to_owned(),
            vec![
                ComboTarget { model: "deepseek-chat".to_owned(), account_id: None, upstream: None, keys: vec![] },
                ComboTarget { model: "claude-sonnet-4-5".to_owned(), account_id: None, upstream: None, keys: vec![] },
            ],
        );
        let mut strategies = HashMap::new();
        strategies.insert("fast-coding".to_owned(), ComboStrategy::FlashFirst);
        let cfg = ComboConfig { combos, strategies };
        if let Ok(text) = serde_json::to_string_pretty(&cfg) {
            let _ = std::fs::write(&combos_path, text);
        }
    }
    println!("Aphelion v3.8 initialized in {}", dir.display());
    println!("  1. Add accounts:  aphelion auth add deepseek-main --provider deepseek --url https://api.deepseek.com --key sk-... --dialect openai --tier cheap");
    println!("  2. Start gateway: aphelion serve --port 18789 --audit");
    println!("  3. Open Web UI:   http://127.0.0.1:18789/ui");
    ExitCode::SUCCESS
}
