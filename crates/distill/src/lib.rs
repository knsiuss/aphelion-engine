//! Offline Flight-Recorder trajectory miner and LoRA/SFT JSONL exporter
//! for Workflow Compilation (distilling stabilized sub-workflows into 0.5B-3B SLMs).

use aphelion_telemetry::audit::{AuditError, AuditLedger, AuditRecord};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use thiserror::Error;
use xxhash_rust::xxh3::xxh3_64;

#[derive(Debug, Error)]
pub enum DistillError {
    #[error("audit ledger error: {0}")]
    Audit(#[from] AuditError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SftExample {
    pub workflow_id: String,
    pub messages: Vec<SftMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SftMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct MinedWorkflow {
    pub workflow_id: String,
    pub occurrences: usize,
    pub examples: Vec<SftExample>,
}

/// Mines recurring, error-free sub-workflows from recorded trajectories.
pub fn mine_stabilized_workflows(
    records: &[AuditRecord],
    min_occurrences: usize,
) -> Vec<MinedWorkflow> {
    let mut clusters: HashMap<u64, Vec<&AuditRecord>> = HashMap::new();

    for rec in records {
        if rec.prompt_text.trim().is_empty() || rec.response_text.trim().is_empty() {
            continue;
        }
        if contains_error_marker(&rec.prompt_text) || contains_error_marker(&rec.response_text) {
            continue;
        }
        let sig = compute_workflow_signature(&rec.prompt_text);
        clusters.entry(sig).or_default().push(rec);
    }

    let mut mined = Vec::new();
    for (sig, group) in clusters {
        if group.len() < min_occurrences {
            continue;
        }
        let workflow_id = format!("wf_{sig:016x}");
        let examples = group
            .iter()
            .map(|r| SftExample {
                workflow_id: workflow_id.clone(),
                messages: vec![
                    SftMessage {
                        role: "user".to_owned(),
                        content: r.prompt_text.clone(),
                    },
                    SftMessage {
                        role: "assistant".to_owned(),
                        content: r.response_text.clone(),
                    },
                ],
            })
            .collect();
        mined.push(MinedWorkflow {
            workflow_id,
            occurrences: group.len(),
            examples,
        });
    }

    mined.sort_by_key(|w| std::cmp::Reverse(w.occurrences));
    mined
}

/// Loads all records from a SQLite Flight Recorder and writes LoRA/SFT JSONL to `writer`.
pub fn export_ledger_to_sft_jsonl<W: Write>(
    ledger: &AuditLedger,
    min_occurrences: usize,
    writer: &mut W,
) -> Result<usize, DistillError> {
    let records = ledger.load_all()?;
    let workflows = mine_stabilized_workflows(&records, min_occurrences);
    export_workflows_jsonl(&workflows, writer)
}

/// Writes mined workflow examples as LoRA/SFT JSONL lines.
pub fn export_workflows_jsonl<W: Write>(
    workflows: &[MinedWorkflow],
    writer: &mut W,
) -> Result<usize, DistillError> {
    let mut written = 0usize;
    for wf in workflows {
        for ex in &wf.examples {
            let line = serde_json::to_string(ex)?;
            writeln!(writer, "{line}")?;
            written += 1;
        }
    }
    Ok(written)
}

fn compute_workflow_signature(prompt: &str) -> u64 {
    // Normalize digits/paths so structurally identical sub-workflows cluster together.
    let normalized: String = prompt
        .chars()
        .take(256)
        .map(|c| {
            if c.is_ascii_digit() {
                '#'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    xxh3_64(normalized.as_bytes())
}

fn contains_error_marker(text: &str) -> bool {
    text.contains("Error:") || text.contains("Traceback") || text.contains("panic:")
}
