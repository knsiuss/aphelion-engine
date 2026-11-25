use aphelion_cli::combo_pool::{ComboConfig, ComboPool, ComboStrategy, ComboTarget};
use aphelion_core::prepare_request;
use aphelion_core::types::{ExecutionProfile, SessionState};
use serde_json::json;
use std::collections::HashMap;

#[test]
fn rtk_filters_grep_with_def_and_chained_bash_but_protects_read() {
    let mut state = SessionState::default();
    let turn0 = json!({
        "model": "test-model",
        "system": [{ "type": "text", "text": "You are a coding assistant.", "cache_control": { "type": "ephemeral" } }],
        "tools": [{ "name": "Grep", "input_schema": { "type": "object" } }, { "name": "Bash", "input_schema": { "type": "object" } }, { "name": "Read", "input_schema": { "type": "object" } }],
        "messages": [{ "role": "user", "content": "Inspect repo" }]
    });
    let _ = prepare_request(turn0.to_string().as_bytes(), ExecutionProfile::Ephemeral, &mut state)
        .expect("turn0");

    let grep_out = (1..=8)
        .map(|i| format!("src/module_a.py:{i}:def compute_metric_{i}(x): return x + {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let chained_out = [
        "On branch main",
        "(use \"git add <file>...\" to update what will be committed)",
        "(use \"git restore <file>...\" to discard changes in working directory)",
        "nothing added to commit (use \"git add\" and/or \"git commit -a\")",
        "commit 1234567890abcdef1234567890abcdef12345678",
        "Author: Dev <dev@example.com>",
        "Date:   Mon Nov 25 10:00:00 2026 +0700",
        "    Initial commit",
        "total 64",
        "-rw-r--r-- 1 user staff  1204 Nov 25 10:00 alpha.py",
        "-rw-r--r-- 1 user staff  2048 Nov 25 10:00 beta.py",
        "-rw-r--r-- 1 user staff  4096 Nov 25 10:00 gamma.py",
    ]
    .join("\n");
    let read_out = "def untouched_source(x):\n    # must remain 100% verbatim\n    return x * 2\n";

    let turn1 = json!({
        "model": "test-model",
        "system": [{ "type": "text", "text": "You are a coding assistant.", "cache_control": { "type": "ephemeral" } }],
        "tools": [{ "name": "Grep", "input_schema": { "type": "object" } }, { "name": "Bash", "input_schema": { "type": "object" } }, { "name": "Read", "input_schema": { "type": "object" } }],
        "messages": [
            { "role": "user", "content": "Inspect repo" },
            { "role": "assistant", "content": [
                { "type": "tool_use", "id": "tu_grep", "name": "Grep", "input": { "pattern": "def " } },
                { "type": "tool_use", "id": "tu_bash", "name": "Bash", "input": { "command": "cd repo && git status && git log -n 5 && ls -la" } },
                { "type": "tool_use", "id": "tu_read", "name": "Read", "input": { "file_path": "src/core.py" } }
            ]},
            { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "tu_grep", "content": grep_out },
                { "type": "tool_result", "tool_use_id": "tu_bash", "content": chained_out },
                { "type": "tool_result", "tool_use_id": "tu_read", "content": read_out, "cache_control": { "type": "ephemeral" } }
            ]}
        ]
    });

    let out = prepare_request(turn1.to_string().as_bytes(), ExecutionProfile::Ephemeral, &mut state)
        .expect("turn1");
    assert!(
        out.metrics.stage_stats.ingress_saved_tokens > 0,
        "expected RTK savings on Grep + chained Bash"
    );
    let body_str = String::from_utf8_lossy(&out.body);
    assert!(body_str.contains("src/module_a.py:"), "grep should be grouped by file");
    assert!(!body_str.contains("(use \\\"git add"), "git status hint should be stripped");
    assert!(body_str.contains("untouched_source"), "Read tool output must remain intact");
}

#[test]
fn flash_first_combo_latches_to_frontier_without_ping_pong() {
    let pool = ComboPool::default();
    let mut combos = HashMap::new();
    combos.insert(
        "fast-coding".to_owned(),
        vec![
            ComboTarget { model: "flash-tier".to_owned(), account_id: None, upstream: None, keys: vec![] },
            ComboTarget { model: "frontier-tier".to_owned(), account_id: None, upstream: None, keys: vec![] },
        ],
    );
    let mut strategies = HashMap::new();
    strategies.insert("fast-coding".to_owned(), ComboStrategy::FlashFirst);
    pool.replace(ComboConfig { combos, strategies });

    // Turn 0 (simple): uses Flash first, Frontier as fallback.
    let s0 = pool.resolve_slots_for_session("combo:fast-coding", &[], Some("sess_a"), false);
    assert_eq!(s0[0].model, "flash-tier");
    assert_eq!(s0[1].model, "frontier-tier");

    // Turn 1 (escalation trigger): switches to Frontier first and latches session.
    let s1 = pool.resolve_slots_for_session("combo:fast-coding", &[], Some("sess_a"), true);
    assert_eq!(s1[0].model, "frontier-tier");
    assert_eq!(s1[1].model, "flash-tier");

    // Turn 2 (subsequent normal turn on same session): stays sticky on Frontier!
    let s2 = pool.resolve_slots_for_session("combo:fast-coding", &[], Some("sess_a"), false);
    assert_eq!(s2[0].model, "frontier-tier");

    // Independent session starts fresh on Flash.
    let s_other = pool.resolve_slots_for_session("combo:fast-coding", &[], Some("sess_b"), false);
    assert_eq!(s_other[0].model, "flash-tier");
}

#[test]
fn consecutive_user_messages_do_not_rollback_rtk_savings() {
    let mut state = SessionState::default();
    let grep_out = (1..=12)
        .map(|i| format!("src/long_module_name.py:{i}:def compute_value_{i}(x): return x + {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let turn0 = json!({
        "model": "test-model",
        "system": [{ "type": "text", "text": "Coding assistant", "cache_control": { "type": "ephemeral" } }],
        "tools": [{ "name": "Grep", "input_schema": { "type": "object" } }],
        "messages": [
            { "role": "user", "content": "<system-reminder>Hook context</system-reminder>" },
            { "role": "user", "content": "Search repo" }
        ]
    });
    let _ = prepare_request(turn0.to_string().as_bytes(), ExecutionProfile::Ephemeral, &mut state)
        .expect("turn0");

    let turn1 = json!({
        "model": "test-model",
        "system": [{ "type": "text", "text": "Coding assistant", "cache_control": { "type": "ephemeral" } }],
        "tools": [{ "name": "Grep", "input_schema": { "type": "object" } }],
        "messages": [
            { "role": "user", "content": "<system-reminder>Hook context</system-reminder>" },
            { "role": "user", "content": "Search repo" },
            { "role": "assistant", "content": [{ "type": "tool_use", "id": "tu_1", "name": "Grep", "input": { "pattern": "def " } }] },
            { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "tu_1", "content": grep_out }] }
        ]
    });
    let raw = turn1.to_string();
    let out = prepare_request(raw.as_bytes(), ExecutionProfile::Ephemeral, &mut state).expect("turn1");
    assert!(!out.metrics.stage_stats.validator_rollback);
    assert!(out.metrics.stage_stats.ingress_saved_tokens > 0);
    assert!(
        out.body.len() < raw.len(),
        "expected outbound bytes ({}) < raw bytes ({}) after RTK without false rollback",
        out.body.len(),
        raw.len()
    );
}
