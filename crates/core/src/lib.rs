//! Sans-I/O Pure Transform Core for Aphelion (`aphelion-core`).
//! Zero network, HTTP, or filesystem I/O is permitted inside this crate.

pub mod canonicalizer;
pub mod compaction;
pub mod composer;
pub mod compressor;
pub mod compressor_rules;
pub mod diet_gc;
pub mod economizer;
pub mod economizer_rules;
pub mod flight_recorder;
pub mod ingress_opt;
pub mod normalizer;
pub mod optical_history;
pub mod output_opt;
pub mod patcher;
pub mod prefix_ledger;
pub mod sse_dialect;
pub mod stream_fsm;
pub mod tron_codec;
pub mod types;
pub mod usage_extract;
pub mod validator;
pub mod verifier;
pub mod workload;

use stream_fsm::StreamFsm;
use thiserror::Error;
use types::{ExecutionProfile, PipelineFlags, SessionState, TransformedPayload, WorkloadClass};

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("invalid JSON payload: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("unsupported API dialect")]
    UnsupportedDialect,
    #[error("cache boundary invariant violated")]
    CacheBoundaryViolation,
    #[error("invalid outbound message graph: {0}")]
    InvalidMessageGraph(String),
}

pub fn prepare_request(
    raw_json: &[u8],
    profile: ExecutionProfile,
    state: &mut SessionState,
) -> Result<TransformedPayload, CoreError> {
    prepare_request_with_flags(raw_json, profile, state, PipelineFlags::all())
}

/// Stage-configurable entry point enforcing Append-Only Prefix Invariance, Turn-0 Output
/// Steering, Pre-Commit Ingress Optimization, and Amortized Macro-Compaction.
pub fn prepare_request_with_flags(
    raw_json: &[u8],
    profile: ExecutionProfile,
    state: &mut SessionState,
    flags: PipelineFlags,
) -> Result<TransformedPayload, CoreError> {
    let mut ir = normalizer::normalize(raw_json, state)?;
    if ir.workload == WorkloadClass::Passthrough {
        return composer::compose_outbound(ir, state, flags);
    }

    // Phase 2: Turn-0 Pre-Freeze Output Steering & Reasoning Budget (BEFORE prefix lock!).
    let opts = state.request_options.clone();
    output_opt::inject_concise_output_directive(&mut ir, &opts);
    output_opt::apply_reasoning_budget(&mut ir, &opts);

    if flags.prefix_lock {
        canonicalizer::apply_prefix_lock(&mut ir, state)?;
    }

    // Cache-Drop Circuit Breaker: lock session into pure `FrozenPrefixPass` mode if tripped.
    let key = state.ledger_key(&ir.model_id);
    let breaker_tripped = state
        .prefix_ledger
        .get(&key)
        .map(|r| r.cache_drop_breaker_tripped)
        .unwrap_or(false);
    if breaker_tripped {
        ir.stage_stats.cache_drop_breaker_active = true;
        return composer::compose_outbound(ir, state, flags);
    }

    let has_new_ingress = ir
        .root
        .get("messages")
        .and_then(|m| m.as_array())
        .map(|a| a.len() > ir.ingress_start)
        .unwrap_or(true);

    match ir.workload {
        WorkloadClass::SingleTurnRagOrChat | WorkloadClass::StructuredOutput => {
            if flags.llmlingua_prose && has_new_ingress {
                compressor::compress_masked_prose(&mut ir, profile, state);
            }
        }
        WorkloadClass::DirectCodeEdit => {
            if flags.llmlingua_prose && has_new_ingress {
                compressor::compress_masked_prose(&mut ir, profile, state);
            }
            if flags.ast_block_priming && has_new_ingress {
                verifier::analyze_and_anchor(&mut ir, state);
            }
        }
        WorkloadClass::AgenticLoop => {
            if flags.tron_codec || flags.diet_gc {
                let mut record = state.prefix_ledger.get_or_insert_mut(key.clone()).clone();
                ingress_opt::optimize_ingress_messages(&mut ir, &mut record, profile);
                state.prefix_ledger.restore_record(key, record);
            }
            if flags.diet_gc {
                compaction::run_amortized_compaction(&mut ir, state);
            }
            if flags.optical_history {
                optical_history::rasterize_if_eligible(&mut ir, state);
            }
            if flags.llmlingua_prose && has_new_ingress {
                compressor::compress_masked_prose(&mut ir, profile, state);
            }
            if flags.economizer_gates {
                economizer::apply_governance(&mut ir, state);
            }
            if flags.ast_block_priming && has_new_ingress {
                verifier::analyze_and_anchor(&mut ir, state);
            }
        }
        WorkloadClass::Passthrough => {}
    }

    composer::compose_outbound(ir, state, flags)
}

pub fn process_sse_chunk(fsm: &mut StreamFsm, chunk: &[u8], session: &mut SessionState) -> Vec<u8> {
    fsm.process_sse_chunk(chunk, session)
}
