# Aphelion Empirical Benchmarks (`BENCHMARKS.md`)

All measurements below were collected offline using exact `tiktoken-rs` BPE tokenization (`o200k_base` / `cl100k_base`), the 9-stage ablation harness (`aphelion-bench-harness`) with **Active-Reference Pinning** and **Zero-HTTP-400 Contract Validation** enabled, and `Criterion` microbenchmarks (`benches/core_latency.rs`) in `--release` mode.

To avoid conflating KV-cache recovery with token compression, every table decomposes savings into two independent columns:
- **Payload Saved:** Pure BPE token reduction savings compared against an **already-cached** baseline.
- **KV-Cache Recovery:** Additional savings recovered from fixing volatile system-prompt cache-busting compared against an **uncanonicalized** baseline.

> [!NOTE]
> `ExecutionProfile::Ephemeral` uses the pure-CPU `LexicalEntropyScorer` (`< 2 ms` budget, `~28%–30%` prose compression on unstructured RAG while keeping fenced code/JSON `0%` mutated). Higher `4x–20x` prose compression requires `ExecutionProfile::LongLived` with optional `--features slm-onnx`.

---

## 1. Reproduction Commands

```bash
# Regenerate deterministic fixtures under bench/fixtures/
cargo run --release -p aphelion-bench-harness -- --generate-fixtures

# Run the 9-stage split-attribution ablation across all 3 fixtures
cargo run --release -p aphelion-bench-harness

# Run Live Shadow A/B replay against any OpenAI / Anthropic / 9Router / vLLM endpoint
cargo run --release -p aphelion-bench-harness -- \
  --live-ab --upstream http://127.0.0.1:8000/v1/chat/completions \
  --api-key-env OPENAI_API_KEY --input bench/fixtures/coding_multi_turn.jsonl

# Run Criterion CPU microbenchmarks (10 KB, 50 KB, 200 KB in Ephemeral profile)
cargo bench -p aphelion-core --bench core_latency

# Live vLLM GPU prefix-cache & TTFT Prometheus probe
cargo run --release -p aphelion-vllm-probe -- --url http://127.0.0.1:8000/metrics
```

---

## 2. 9-Stage Offline Ablation Results

Pricing model for Effective Cost ($C_{\text{eff}}$): \$3.00 / 1M input tokens, \$15.00 / 1M output tokens, `1.25x` cache write, `0.10x` cache read.

### Fixture A: `bench/fixtures/coding_multi_turn.jsonl`
*12-turn coding trajectory with superseded `read_file` calls, Active-Reference Pinning on `src/engine.rs`, failing test stacktraces, and 200-line file edits.*

| Stage | Raw Tok | Opt Tok | Payload Saved | KV-Cache Recovery | $S_{\text{prefix}}$ | Tools | $C_{\text{eff}}$ (USD) |
| :--- | ---: | ---: | ---: | ---: | ---: | :---: | ---: |
| Baseline | 228,199 | 228,199 | 0.0% (\$0.0000) | 0.0% (\$0.0000) | 1.00 | 100% | \$0.68460 |
| +PrefixLock | 228,199 | 228,082 | 0.1% (\$0.0004) | 0.4% (\$0.0024) | 1.00 | 100% | \$0.68181 |
| +AgentDiet_GC | 228,199 | 41,674 | 82.0% (\$0.5596) | 0.4% (\$0.0024) | 1.00 | 100% | \$0.12259 |
| +LLMLingua_Prose | 228,199 | 41,670 | 82.0% (\$0.5596) | 0.4% (\$0.0024) | 1.00 | 100% | \$0.12257 |
| +TRON_Codec | 228,199 | 41,670 | 82.0% (\$0.5596) | 0.4% (\$0.0024) | 1.00 | 100% | \$0.12257 |
| +AgentOCR_History | 228,199 | 41,670 | 82.0% (\$0.5596) | 0.4% (\$0.0024) | 1.00 | 100% | \$0.12257 |
| +TokenEconomizer_Gates | 228,199 | 41,999 | 81.9% (\$0.5586) | 0.4% (\$0.0024) | 1.00 | 100% | \$0.12356 |
| +AST_Block_Priming | 228,199 | 41,999 | 81.9% (\$0.5586) | 0.4% (\$0.0024) | 1.00 | 100% | \$0.12356 |
| **Full Symbiotic** | **228,199** | **41,999** | **81.9% (\$0.5586)** | **0.4% (\$0.0024)** | **1.00** | **100%** | **\$0.12356** |

### Fixture B: `bench/fixtures/mcp_tool_heavy.jsonl`
*25 active MCP tools (100% immutable), 30-row homogeneous JSON array in `tool_result` (TRON), and 3x identical tool failure loop (atomic pair collapse + Circuit Breaker pivot).*

| Stage | Raw Tok | Opt Tok | Payload Saved | KV-Cache Recovery | $S_{\text{prefix}}$ | Tools | $C_{\text{eff}}$ (USD) |
| :--- | ---: | ---: | ---: | ---: | ---: | :---: | ---: |
| Baseline | 7,191 | 7,191 | 0.0% (\$0.0000) | 0.0% (\$0.0000) | 1.00 | 100% | \$0.02157 |
| +PrefixLock | 7,191 | 7,201 | 0.0% (\$0.0000) | 36.3% (\$0.0078) | 1.00 | 100% | \$0.01376 |
| +AgentDiet_GC | 7,191 | 7,201 | 0.0% (\$0.0000) | 36.3% (\$0.0078) | 1.00 | 100% | \$0.01376 |
| +LLMLingua_Prose | 7,191 | 7,201 | 0.0% (\$0.0000) | 36.3% (\$0.0078) | 1.00 | 100% | \$0.01376 |
| +TRON_Codec | 7,191 | 6,908 | 6.2% (\$0.0008) | 36.3% (\$0.0078) | 1.00 | 100% | \$0.01288 |
| +AgentOCR_History | 7,191 | 6,908 | 6.2% (\$0.0008) | 36.3% (\$0.0078) | 1.00 | 100% | \$0.01288 |
| +TokenEconomizer_Gates | 7,191 | 6,960 | 5.0% (\$0.0007) | 36.3% (\$0.0078) | 1.00 | 100% | \$0.01304 |
| +AST_Block_Priming | 7,191 | 6,960 | 5.0% (\$0.0007) | 36.3% (\$0.0078) | 1.00 | 100% | \$0.01304 |
| **Full Symbiotic** | **7,191** | **6,960** | **5.0% (\$0.0007)** | **36.3% (\$0.0078)** | **1.00** | **100%** | **\$0.01304** |

### Fixture C: `bench/fixtures/rag_long_context.jsonl`
*Multi-chunk documentation with needle-in-the-middle relevance placement + protected fenced Rust code block.*

| Stage | Raw Tok | Opt Tok | Payload Saved | KV-Cache Recovery | $S_{\text{prefix}}$ | Tools | $C_{\text{eff}}$ (USD) |
| :--- | ---: | ---: | ---: | ---: | ---: | :---: | ---: |
| Baseline | 1,748 | 1,748 | 0.0% (\$0.0000) | 0.0% (\$0.0000) | 1.00 | 100% | \$0.00524 |
| +PrefixLock | 1,748 | 1,747 | 0.1% (\$0.0000) | 0.0% (\$0.0000) | 1.00 | 100% | \$0.00524 |
| +AgentDiet_GC | 1,748 | 1,747 | 0.1% (\$0.0000) | 0.0% (\$0.0000) | 1.00 | 100% | \$0.00524 |
| +LLMLingua_Prose | 1,748 | 1,232 | 29.5% (\$0.0015) | 0.0% (\$0.0000) | 1.00 | 100% | \$0.00370 |
| +TRON_Codec | 1,748 | 1,232 | 29.5% (\$0.0015) | 0.0% (\$0.0000) | 1.00 | 100% | \$0.00370 |
| +AgentOCR_History | 1,748 | 1,232 | 29.5% (\$0.0015) | 0.0% (\$0.0000) | 1.00 | 100% | \$0.00370 |
| +TokenEconomizer_Gates | 1,748 | 1,232 | 29.5% (\$0.0015) | 0.0% (\$0.0000) | 1.00 | 100% | \$0.00370 |
| +AST_Block_Priming | 1,748 | 1,255 | 28.2% (\$0.0015) | 0.0% (\$0.0000) | 1.00 | 100% | \$0.00377 |
| **Full Symbiotic** | **1,748** | **1,255** | **28.2% (\$0.0015)** | **0.0% (\$0.0000)** | **1.00** | **100%** | **\$0.00377** |

---

## 3. CPU Microbenchmark Latency (`ExecutionProfile::Ephemeral`)

Measured with `Criterion 0.5` (`cargo bench -p aphelion-core --bench core_latency`), 100 samples per payload size on Windows x86_64:

| Payload Size | Lower Bound | Median (p50) | Upper Bound (p99 CI) | Budget Target |
| :--- | ---: | ---: | ---: | :---: |
| **10 KB** | 262.89 µs | **266.06 µs** | 269.89 µs | `< 3 ms` (PASS) |
| **50 KB** | 1.3355 ms | **1.3547 ms** | 1.3771 ms | `< 3 ms` (PASS) |
| **200 KB** | 5.3659 ms | **5.5103 ms** | 5.6712 ms | Linear $O(N)$ |
