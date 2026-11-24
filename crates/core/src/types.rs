use crate::prefix_ledger::PrefixLedger;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionProfile {
    /// Short-lived CLI wrapper: forbids ONNX cold-start initialization (< 2ms CPU budget).
    Ephemeral,
    /// Persistent router middleware or SDK: permits warm INT8 ONNX models in memory.
    LongLived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dialect {
    OpenAiChat,
    OpenAiResponses,
    AnthropicMessages,
    GeminiGenerate,
    OllamaNative,
    Passthrough,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkloadClass {
    AgenticLoop,
    DirectCodeEdit,
    SingleTurnRagOrChat,
    StructuredOutput,
    Passthrough,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Complexity {
    Trivial,
    Deep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodeLang {
    Rust,
    TypeScript,
    JavaScript,
    Python,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineFlags {
    pub prefix_lock: bool,
    pub diet_gc: bool,
    pub llmlingua_prose: bool,
    pub tron_codec: bool,
    pub optical_history: bool,
    pub economizer_gates: bool,
    pub ast_block_priming: bool,
}

impl PipelineFlags {
    pub const fn none() -> Self {
        Self {
            prefix_lock: false,
            diet_gc: false,
            llmlingua_prose: false,
            tron_codec: false,
            optical_history: false,
            economizer_gates: false,
            ast_block_priming: false,
        }
    }

    pub const fn all() -> Self {
        Self {
            prefix_lock: true,
            diet_gc: true,
            llmlingua_prose: true,
            tron_codec: true,
            optical_history: true,
            economizer_gates: true,
            ast_block_priming: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockSpan {
    pub block_id: String,
    pub file_hint: String,
    pub language: CodeLang,
    pub byte_start: usize,
    pub byte_end: usize,
    pub original_source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SlmRouteDecision {
    #[default]
    Frontier,
    CompiledSlm {
        workflow_id: String,
        endpoint: String,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StageStats {
    pub diet_gc_compacted_tokens: usize,
    pub ingress_saved_tokens: usize,
    pub optical_tiles_emitted: usize,
    pub prose_pruned_tokens: usize,
    pub tron_tables_encoded: usize,
    pub circuit_breaker_tripped: bool,
    pub cache_drop_breaker_active: bool,
    pub macro_compaction_triggered: bool,
    pub micro_skill_injected: Option<String>,
    pub blocks_anchored: usize,
    pub validator_rollback: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamUsage {
    pub input_tokens: usize,
    pub uncached_input_tokens: usize,
    pub output_tokens: usize,
    pub cached_tokens: usize,
    pub cache_creation_tokens: usize,
    pub reasoning_tokens: usize,
}

#[derive(Debug, Clone, Default)]
pub struct SessionState {
    pub session_id: Option<u128>,
    pub turn_alias: Option<u64>,
    pub turn_index: usize,
    pub frozen_prefix_len: usize,
    pub frozen_prefix_bytes: Vec<u8>,
    pub sacred_cache_boundary: usize,
    pub last_thought_anchor: Option<String>,
    pub last_usage: Option<UpstreamUsage>,
    pub known_blocks: HashMap<String, BlockSpan>,
    pub recent_action_sigs: Vec<u64>,
    pub recent_error_sigs: Vec<u64>,
    pub workflow_counts: HashMap<u64, u32>,
    pub active_micro_skill: Option<String>,
    pub prefix_ledger: PrefixLedger,
    pub active_model: String,
}

impl SessionState {
    pub fn record_upstream_usage(&mut self, usage: &UpstreamUsage) {
        let key = PrefixLedger::active_key(self.session_id, &self.active_model);
        let rec = self.prefix_ledger.get_or_insert_mut(key);
        rec.record_usage(
            usage.input_tokens,
            usage.cached_tokens,
            usage.cache_creation_tokens,
        );
    }
}

#[derive(Debug, Clone)]
pub struct NormalizedRequest {
    pub dialect: Dialect,
    pub workload: WorkloadClass,
    pub model_id: String,
    pub root: Value,
    pub raw_passthrough: Option<Vec<u8>>,
    pub sacred_boundary: usize,
    pub is_turn_zero: bool,
    pub ingress_start: usize,
    pub active_tool_loop: bool,
    pub complexity: Complexity,
    pub supports_prefill: bool,
    pub supports_vision: bool,
    pub tail_directives: Vec<String>,
    pub assistant_prefill: Option<String>,
    pub slm_route: SlmRouteDecision,
    pub stage_stats: StageStats,
    pub pre_compaction_messages: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransformMetrics {
    pub session_id_hex: String,
    pub turn_alias: Option<u64>,
    pub dialect: Dialect,
    pub workload: WorkloadClass,
    pub frozen_prefix_len: usize,
    pub complexity_deep: bool,
    pub active_tool_loop: bool,
    pub prefill_injected: bool,
    pub slm_route: SlmRouteDecision,
    pub stage_stats: StageStats,
}

#[derive(Debug, Clone)]
pub struct TransformedPayload {
    pub body: Vec<u8>,
    pub metrics: TransformMetrics,
}
