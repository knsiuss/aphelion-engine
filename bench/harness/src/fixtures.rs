use serde_json::{json, Value};
use std::fs;
use std::path::Path;

/// Generates the 3 deterministic benchmark trajectory fixtures under `bench/fixtures/`.
pub fn write_all_fixtures(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    write_jsonl(&dir.join("coding_multi_turn.jsonl"), &coding_multi_turn())?;
    write_jsonl(&dir.join("mcp_tool_heavy.jsonl"), &mcp_tool_heavy())?;
    write_jsonl(&dir.join("rag_long_context.jsonl"), &rag_long_context())?;
    Ok(())
}

fn write_jsonl(path: &Path, items: &[Value]) -> std::io::Result<()> {
    let mut out = String::new();
    for item in items {
        if let Ok(line) = serde_json::to_string(item) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    fs::write(path, out)
}

/// 12-turn coding trajectory with superseded `read_file` calls, failing test stacktraces,
/// and 200-line file edits.
pub fn coding_multi_turn() -> Vec<Value> {
    let sys = "You are a principal Rust engineer. cwd: /repo/aphelion Time: 2026-11-24T12:00:00Z";
    let tools = json!([
        {"type": "function", "function": {"name": "read_file", "description": "Read file"}},
        {"type": "function", "function": {"name": "edit_file", "description": "Edit file"}},
        {"type": "function", "function": {"name": "run_tests", "description": "Run cargo test"}}
    ]);
    let big_file: String = (1..=210)
        .map(|i| format!("pub fn compute_step_{i}(val: usize) -> usize {{ val.wrapping_add({i}) }}\n"))
        .collect();
    let old_log = "compiling crate module step check ok warning unused_var line 42\n".repeat(450);

    let mut turns = Vec::with_capacity(12);
    let mut history: Vec<Value> = Vec::new();

    for t in 0..12 {
        match t {
            0 => history.push(json!({"role": "user", "content": "Please basically inspect src/engine.rs and fix the overflow bug."})),
            1 => {
                history.push(json!({"role": "assistant", "content": "Reading src/engine.rs.", "tool_calls": [{"id": "c1", "function": {"name": "read_file", "arguments": "{\"path\":\"src/engine.rs\"}"}}]}));
                history.push(json!({"role": "tool", "tool_call_id": "c1", "name": "read_file", "content": format!("File: src/engine.rs\n{big_file}")}));
            }
            2 => {
                history.push(json!({"role": "assistant", "content": "Checking build logs.", "tool_calls": [{"id": "c2", "function": {"name": "run_tests", "arguments": "{\"cmd\":\"cargo test\"}"}}]}));
                history.push(json!({"role": "tool", "tool_call_id": "c2", "name": "run_tests", "content": old_log}));
            }
            3 => {
                history.push(json!({"role": "assistant", "content": "Applying initial edit.", "tool_calls": [{"id": "c3", "function": {"name": "edit_file", "arguments": "{\"path\":\"src/engine.rs\"}"}}]}));
                history.push(json!({"role": "tool", "tool_call_id": "c3", "name": "edit_file", "content": "Edited src/engine.rs successfully."}));
            }
            4 => {
                history.push(json!({"role": "assistant", "content": "Running tests.", "tool_calls": [{"id": "c4", "function": {"name": "run_tests", "arguments": "{\"cmd\":\"cargo test\"}"}}]}));
                history.push(json!({"role": "tool", "tool_call_id": "c4", "name": "run_tests", "content": "Error: panicked at src/engine.rs:88: attempt to add with overflow\nFAILED"}));
            }
            5..=8 => {
                history.push(json!({"role": "assistant", "content": format!("Refining step {t}."), "tool_calls": [{"id": format!("c{t}"), "function": {"name": "read_file", "arguments": "{\"path\":\"src/engine.rs\"}"}}]}));
                history.push(json!({"role": "tool", "tool_call_id": format!("c{t}"), "name": "read_file", "content": format!("File: src/engine.rs\n{big_file}")}));
            }
            9 => {
                history.push(json!({"role": "assistant", "content": "Fixing overflow.", "tool_calls": [{"id": "c9", "function": {"name": "edit_file", "arguments": "{\"path\":\"src/engine.rs\"}"}}]}));
                history.push(json!({"role": "tool", "tool_call_id": "c9", "name": "edit_file", "content": "Applied saturating_add fix."}));
            }
            10 => {
                history.push(json!({"role": "assistant", "content": "Verifying tests.", "tool_calls": [{"id": "c10", "function": {"name": "run_tests", "arguments": "{\"cmd\":\"cargo test\"}"}}]}));
                history.push(json!({"role": "tool", "tool_call_id": "c10", "name": "run_tests", "content": "test result: ok. 14 passed; 0 failed"}));
            }
            _ => {
                history.push(json!({"role": "user", "content": "Great, summarize the fix concisely."}));
            }
        }
        turns.push(json!({
            "model": "gpt-4o",
            "system": sys,
            "tools": tools,
            "messages": history
        }));
    }
    turns
}

/// 25 active MCP tools, homogeneous JSON arrays in `tool_result` (TRON),
/// and a 3x identical tool failure loop (Circuit Breaker).
pub fn mcp_tool_heavy() -> Vec<Value> {
    let tools: Vec<Value> = (1..=25)
        .map(|i| {
            json!({
                "type": "function",
                "function": {
                    "name": format!("mcp_tool_{i:02}"),
                    "description": format!("MCP domain action number {i} for workspace automation")
                }
            })
        })
        .collect();
    let rows: Vec<Value> = (1..=30)
        .map(|i| json!({"id": i, "file": format!("crates/mod_{i}.rs"), "lines": 100 + i, "status": "ok"}))
        .collect();
    let tron_payload = serde_json::to_string(&rows).unwrap_or_default();

    let mut turns = Vec::new();
    let mut history = vec![json!({"role": "user", "content": "Query workspace index via mcp_tool_01 and update schema."})];
    turns.push(json!({"model": "gpt-4o", "system": "MCP Agent cwd: /repo/aphelion", "tools": tools, "messages": history}));

    history.push(json!({"role": "assistant", "content": "Querying.", "tool_calls": [{"id": "t1", "function": {"name": "mcp_tool_01", "arguments": "{\"q\":\"all\"}"}}]}));
    history.push(json!({"role": "tool", "tool_call_id": "t1", "name": "mcp_tool_01", "content": tron_payload}));
    turns.push(json!({"model": "gpt-4o", "system": "MCP Agent cwd: /repo/aphelion", "tools": tools, "messages": history}));

    for rep in 1..=3 {
        history.push(json!({"role": "assistant", "content": "Retrying edit.", "tool_calls": [{"id": format!("r{rep}"), "function": {"name": "mcp_tool_02", "arguments": "{\"target\":\"schema.sql\"}"}}]}));
        history.push(json!({"role": "tool", "tool_call_id": format!("r{rep}"), "name": "mcp_tool_02", "content": "Error: permission denied on schema.sql constraint check FAILED"}));
        turns.push(json!({"model": "gpt-4o", "system": "MCP Agent cwd: /repo/aphelion", "tools": tools, "messages": history}));
    }
    turns
}

/// Multi-chunk RAG documentation with needle-in-the-middle relevance + protected fenced code blocks.
pub fn rag_long_context() -> Vec<Value> {
    let chunk_filler = "Please kindly note that this generic background documentation paragraph basically describes legacy deployment steps and historical architecture notes in great detail. ".repeat(18);
    let needle = "CRITICAL: `aphelion` uses `frozen_prefix_len` to lock KV-cache prefix bytes so RadixAttention never invalidates cached blocks.\n```rust\nfn verify_prefix(a: &[u8], b: &[u8], n: usize) -> bool {\n    a.len() >= n && b.len() >= n && a[..n] == b[..n]\n}\n```";
    let rag_doc = format!(
        "{chunk_filler}\n---\n{chunk_filler}\n---\n{needle}\n---\n{chunk_filler}\n---\n{chunk_filler}\n---\nExplain how `aphelion` locks `frozen_prefix_len` for RadixAttention."
    );
    vec![json!({
        "model": "gpt-4o",
        "system": "You are a documentation assistant. cwd: /repo/aphelion",
        "messages": [{"role": "user", "content": rag_doc}]
    })]
}
