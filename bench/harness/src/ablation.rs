use aphelion_core::canonicalizer::verify_prefix_invariant;
use aphelion_core::types::{ExecutionProfile, PipelineFlags, SessionState};
use aphelion_telemetry::bpe::count_payload_tokens;
use aphelion_telemetry::ledger::{PricingRates, SessionCostSummary, TurnTelemetry};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct AblationStage {
    pub name: &'static str,
    pub flags: PipelineFlags,
}

#[derive(Debug, Clone)]
pub struct AblationRow {
    pub stage_name: &'static str,
    pub summary: SessionCostSummary,
    pub payload_saved_usd: f64,
    pub payload_saved_pct: f64,
    pub kv_recovery_usd: f64,
    pub kv_recovery_pct: f64,
    pub prefix_stability: f64,
    pub tool_schema_parity: bool,
}

pub fn nine_stage_ablation_matrix() -> Vec<AblationStage> {
    let base = PipelineFlags::none();
    let s1 = PipelineFlags {
        prefix_lock: true,
        ..base
    };
    let s2 = PipelineFlags {
        diet_gc: true,
        ..s1
    };
    let s3 = PipelineFlags {
        llmlingua_prose: true,
        ..s2
    };
    let s4 = PipelineFlags {
        tron_codec: true,
        ..s3
    };
    let s5 = PipelineFlags {
        optical_history: true,
        ..s4
    };
    let s6 = PipelineFlags {
        economizer_gates: true,
        ..s5
    };
    let s7 = PipelineFlags {
        ast_block_priming: true,
        ..s6
    };
    vec![
        AblationStage {
            name: "Baseline",
            flags: base,
        },
        AblationStage {
            name: "+PrefixLock",
            flags: s1,
        },
        AblationStage {
            name: "+AgentDiet_GC",
            flags: s2,
        },
        AblationStage {
            name: "+LLMLingua_Prose",
            flags: s3,
        },
        AblationStage {
            name: "+TRON_Codec",
            flags: s4,
        },
        AblationStage {
            name: "+AgentOCR_History",
            flags: s5,
        },
        AblationStage {
            name: "+TokenEconomizer_Gates",
            flags: s6,
        },
        AblationStage {
            name: "+AST_Block_Priming",
            flags: s7,
        },
        AblationStage {
            name: "Full Symbiotic",
            flags: PipelineFlags::all(),
        },
    ]
}

pub fn run_ablation(trajectory: &[Vec<u8>]) -> Vec<AblationRow> {
    let rates = PricingRates::default();
    let mut results = Vec::new();

    for stage in nine_stage_ablation_matrix() {
        let mut state = SessionState::default();
        let mut summary = SessionCostSummary::default();
        let mut prev_out: Option<Vec<u8>> = None;
        let mut prefix_checks = 0usize;
        let mut prefix_hits = 0usize;
        let mut tool_parity = true;
        let mut already_cached_baseline_usd = 0.0f64;

        for raw in trajectory {
            let model = extract_model(raw);
            let raw_tok = count_payload_tokens(&model, raw);
            let raw_tools = extract_tools(raw);

            let (opt_tok, cache_read) = match aphelion_core::prepare_request_with_flags(
                raw,
                ExecutionProfile::Ephemeral,
                &mut state,
                stage.flags,
            ) {
                Ok(out) => {
                    if extract_tools(&out.body) != raw_tools {
                        tool_parity = false;
                    }
                    if let Some(prev) = &prev_out {
                        if state.frozen_prefix_len > 0 {
                            prefix_checks += 1;
                            if verify_prefix_invariant(prev, &out.body, state.frozen_prefix_len) {
                                prefix_hits += 1;
                            }
                        }
                    }
                    prev_out = Some(out.body.clone());
                    let opt = count_payload_tokens(&model, &out.body);
                    let cached = if stage.flags.prefix_lock && state.turn_index > 1 {
                        count_payload_tokens(&model, &state.frozen_prefix_bytes)
                    } else {
                        0
                    };
                    (opt, cached)
                }
                Err(_) => (raw_tok, 0),
            };

            let turn = TurnTelemetry {
                raw_input_tokens: raw_tok,
                opt_input_tokens: opt_tok,
                cache_read_tokens: cache_read,
                ..TurnTelemetry::default()
            };
            let cached_base_turn = TurnTelemetry {
                raw_input_tokens: raw_tok,
                opt_input_tokens: raw_tok,
                cache_read_tokens: cache_read,
                ..TurnTelemetry::default()
            };
            already_cached_baseline_usd += cached_base_turn.effective_cost_usd(rates);
            summary.record(&turn, rates);
        }

        let payload_saved_usd = (already_cached_baseline_usd - summary.optimized_cost_usd).max(0.0);
        let payload_saved_pct = if already_cached_baseline_usd > 0.0 {
            (payload_saved_usd * 100.0) / already_cached_baseline_usd
        } else {
            0.0
        };
        let kv_recovery_usd = (summary.baseline_cost_usd - already_cached_baseline_usd).max(0.0);
        let kv_recovery_pct = if summary.baseline_cost_usd > 0.0 {
            (kv_recovery_usd * 100.0) / summary.baseline_cost_usd
        } else {
            0.0
        };
        let stability = if prefix_checks == 0 {
            1.0
        } else {
            prefix_hits as f64 / prefix_checks as f64
        };
        results.push(AblationRow {
            stage_name: stage.name,
            summary,
            payload_saved_usd,
            payload_saved_pct,
            kv_recovery_usd,
            kv_recovery_pct,
            prefix_stability: stability,
            tool_schema_parity: tool_parity,
        });
    }
    results
}

fn extract_model(raw: &[u8]) -> String {
    serde_json::from_slice::<Value>(raw)
        .ok()
        .and_then(|v| v.get("model").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| "gpt-4o".to_owned())
}

fn extract_tools(raw: &[u8]) -> Option<Value> {
    let mut val = serde_json::from_slice::<Value>(raw)
        .ok()?
        .get("tools")?
        .clone();
    if let Some(arr) = val.as_array_mut() {
        arr.sort_by(|a, b| {
            let na = a
                .pointer("/function/name")
                .or_else(|| a.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let nb = b
                .pointer("/function/name")
                .or_else(|| b.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            na.cmp(nb)
        });
    }
    Some(val)
}
