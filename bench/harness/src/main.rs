mod ablation;
mod fixtures;
mod live_ab;

use ablation::{run_ablation, AblationRow};
use std::env;
use std::fs;
use std::path::Path;

fn load_jsonl(path: &Path) -> Vec<Vec<u8>> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.as_bytes().to_vec())
        .collect()
}

fn print_table(label: &str, rows: &[AblationRow]) {
    println!("\n### Fixture: `{label}`");
    println!("| Stage | Raw Tok | Opt Tok | Payload Saved | KV-Cache Recovery | S_prefix | Tools | C_eff (USD) |");
    println!("| :--- | ---: | ---: | ---: | ---: | ---: | :---: | ---: |");
    for r in rows {
        println!(
            "| {:<22} | {:>7} | {:>7} | {:>5.1}% (${:.4}) | {:>5.1}% (${:.4}) | {:>8.2} | {:^5} | ${:>9.5} |",
            r.stage_name,
            r.summary.total_raw_tokens,
            r.summary.total_opt_tokens,
            r.payload_saved_pct,
            r.payload_saved_usd,
            r.kv_recovery_pct,
            r.kv_recovery_usd,
            r.prefix_stability,
            if r.tool_schema_parity { "100%" } else { "FAIL" },
            r.summary.optimized_cost_usd
        );
    }
}

fn arg_val<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|w| w[0] == flag)
        .map(|w| w[1].as_str())
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|a| a == "--generate-fixtures") {
        let dir = Path::new("bench/fixtures");
        if let Err(err) = fixtures::write_all_fixtures(dir) {
            eprintln!("failed generating fixtures: {err}");
            std::process::exit(1);
        }
        println!("Generated fixtures in {}", dir.display());
        return;
    }

    if args.iter().any(|a| a == "--live-ab") {
        let upstream =
            arg_val(&args, "--upstream").unwrap_or("http://127.0.0.1:8000/v1/chat/completions");
        let api_key_env = arg_val(&args, "--api-key-env").unwrap_or("OPENAI_API_KEY");
        let model = arg_val(&args, "--model");
        let traj = match arg_val(&args, "--input") {
            Some(p) => load_jsonl(Path::new(p)),
            None => fixtures::rag_long_context()
                .iter()
                .filter_map(|v| serde_json::to_vec(v).ok())
                .collect(),
        };
        live_ab::run_live_ab(upstream, api_key_env, model, &traj).await;
        return;
    }

    if let Some(p) = arg_val(&args, "--input") {
        let traj = load_jsonl(Path::new(p));
        print_table(p, &run_ablation(&traj));
        return;
    }

    for (name, items) in [
        ("coding_multi_turn.jsonl", fixtures::coding_multi_turn()),
        ("mcp_tool_heavy.jsonl", fixtures::mcp_tool_heavy()),
        ("rag_long_context.jsonl", fixtures::rag_long_context()),
    ] {
        let traj: Vec<Vec<u8>> = items
            .iter()
            .filter_map(|v| serde_json::to_vec(v).ok())
            .collect();
        print_table(name, &run_ablation(&traj));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nine_stage_ablation_reduces_tokens_and_preserves_invariants() {
        let traj: Vec<Vec<u8>> = fixtures::coding_multi_turn()
            .iter()
            .filter_map(|v| serde_json::to_vec(v).ok())
            .collect();
        let rows = run_ablation(&traj);
        assert_eq!(rows.len(), 9);
        let full = &rows[8];
        assert!(full.summary.total_opt_tokens < rows[0].summary.total_raw_tokens);
        assert_eq!(full.prefix_stability, 1.0);
        assert!(full.tool_schema_parity);
    }
}
