//! Sans-I/O Pure Transform Core for Aphelion (`aphelion-core`).
//! Zero network, HTTP, or filesystem I/O is permitted inside this crate.

pub mod canonicalizer;
pub mod composer;
pub mod compressor;
pub mod diet_gc;
pub mod economizer;
pub mod economizer_rules;
pub mod normalizer;
pub mod optical_history;
pub mod patcher;
pub mod sse_dialect;
pub mod stream_fsm;
pub mod tron_codec;
pub mod types;
pub mod validator;
pub mod verifier;

use stream_fsm::StreamFsm;
use thiserror::Error;
use types::{ExecutionProfile, PipelineFlags, SessionState, TransformedPayload};

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

/// Primary entry point for transforming an outbound LLM request with all stages enabled.
pub fn prepare_request(
    raw_json: &[u8],
    profile: ExecutionProfile,
    state: &mut SessionState,
) -> Result<TransformedPayload, CoreError> {
    prepare_request_with_flags(raw_json, profile, state, PipelineFlags::all())
}

/// Stage-configurable entry point used by both production and the 9-stage ablation harness.
pub fn prepare_request_with_flags(
    raw_json: &[u8],
    profile: ExecutionProfile,
    state: &mut SessionState,
    flags: PipelineFlags,
) -> Result<TransformedPayload, CoreError> {
    let mut ir = normalizer::normalize(raw_json, state)?;
    if flags.prefix_lock {
        canonicalizer::apply_prefix_lock(&mut ir, state)?;
    }
    if flags.diet_gc {
        diet_gc::compact_if_amortized(&mut ir, state);
    }
    if flags.optical_history {
        optical_history::rasterize_if_eligible(&mut ir, state);
    }
    if flags.llmlingua_prose {
        compressor::compress_masked_prose(&mut ir, profile, state);
    }
    if flags.tron_codec {
        tron_codec::transcode_inbound_tool_results(&mut ir);
    }
    if flags.economizer_gates {
        economizer::apply_governance(&mut ir, state);
    }
    if flags.ast_block_priming {
        verifier::analyze_and_anchor(&mut ir, state);
    }
    composer::compose_outbound(ir, state, flags)
}

/// Incremental SSE chunk processor wrapping `StreamFsm` for wire-level adapters.
pub fn process_sse_chunk(
    fsm: &mut StreamFsm,
    chunk: &[u8],
    session: &mut SessionState,
) -> Vec<u8> {
    fsm.process_sse_chunk(chunk, session)
}
