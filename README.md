# Aphelion (`aphelion`) v1.0

**Sans-I/O Symbiotic Token & Inference Optimization Engine for Coding Agents and Reasoning LLMs.**

`aphelion` sits transparently at the wire-transformation layer (`Layer 3`) between coding agents (Claude Code, OpenCode, Cline, Aider) and LLM routers/backends (9Router, LiteLLM, vLLM, SGLang, Anthropic, OpenAI, DeepSeek). Instead of fighting model probability distributions with negative prompts or cache-busting history rewrites, it aligns payload bytes with **PagedAttention / RadixAttention KV-caching, RoPE attention windows, ViT patch caches, BPE subword tokenization, and Zero-HTTP-400 message-graph invariants**.

```mermaid
flowchart LR
    Agent[Coding Agent\nClaude Code / OpenCode / Cline] -->|HTTP/1.1 + gzip/chunked| Proxy[Ephemeral CLI Proxy\n127.0.0.1:0 | Profile: Ephemeral]
    Proxy --> Core[Pure Sans-I/O Core\nPrefixLock -> DietGC -> TRON -> Economizer -> Validator]
    Core -->|S_prefix = 1.00| Upstream[(9Router / LiteLLM / vLLM / Frontier API)]
    Upstream -->|SSE: OpenAI / DeepSeek / Anthropic| FSM[Zero-Panic StreamFsm\nThink Distiller + Usage + AST Patcher]
    FSM --> Agent
    Proxy -.->|--audit| SQLite[(aphelion_audit.db\nVerified Usage & Cache Hits)]
```

---

## 30-Second Quickstart

```bash
# 1. Wrap any coding agent CLI (zero background daemon, SQLite audit enabled)
npx aphelion --audit claude
# or via Cargo:
cargo run --release -p aphelion-cli -- --audit claude

# 2. Inspect verified upstream cache hits, BPE token savings, and net $ saved
aphelion --audit-summary

# 3. Export recorded trajectories for offline or live Shadow A/B benchmarking
aphelion --audit-export session.jsonl
cargo run --release -p aphelion-bench-harness -- --input session.jsonl
```

### In-Process Router Middleware (`@aphelion/middleware` for 9Router / Node)

```js
import { createAphelionMiddleware } from "@aphelion/middleware";

const aphelion = createAphelionMiddleware({ profile: "LongLived" });
const optimizedBody = aphelion.transformRequest(rawJsonBytes, { sessionId });
const optimizedSse = aphelion.transformSseStream(upstreamResponse.body, { sessionId });
```

---

## Empirical Benchmark Summary ([BENCHMARKS.md](./BENCHMARKS.md))

| Fixture | Raw BPE Tok | Opt BPE Tok | Payload Saved | KV-Cache Recovery | $S_{\text{prefix}}$ | Tools Parity |
| :--- | ---: | ---: | ---: | ---: | ---: | :---: |
| `coding_multi_turn.jsonl` (12 turns, Active Pinning) | 228,199 | 41,999 | **81.9%** (\$0.5586) | **0.4%** (\$0.0024) | **1.00** | **100%** |
| `mcp_tool_heavy.jsonl` (25 tools + TRON + 3x loop) | 7,191 | 6,960 | **5.0%** (\$0.0007) | **36.3%** (\$0.0078) | **1.00** | **100%** |
| `rag_long_context.jsonl` (RAG + code mask) | 1,748 | 1,255 | **28.2%** (\$0.0015) | 0.0% (\$0.0000) | **1.00** | **100%** |

- **Criterion CPU Latency (`ExecutionProfile::Ephemeral`):** `266 µs` (10 KB) | **`1.35 ms` median / `1.38 ms` p99 CI (50 KB, `< 3 ms` budget PASS)** | `5.51 ms` (200 KB).

---

## The 6 Non-Negotiable Hard Invariants

1. **Zero Prefix Mutation:** Bytes `< frozen_prefix_len` and Anthropic `cache_control` spans are 100% immutable ($S_{\text{prefix}} = 1.00$).
2. **Prefix-Safe Soft Tool-Gating Only:** `tools[]` schemas are never truncated or deleted after Turn-0; active tool narrowing happens solely via RoPE tail directives, with prefill/`@patch:@block` disabled in active tool loops.
3. **BPE-Calibrated Cache Amortization:** Uses exact/calibrated BPE tokenizers (never `bytes / 4`) with `4,000`-token `AgentDiet` (plus Active-Reference Pinning) and `6,000`-token `AgentOCR` watermarks.
4. **Lifecycle Cold-Start Guard:** `ExecutionProfile::Ephemeral` never initializes ONNX Runtime (`< 2 ms` pure-CPU lexical scorer).
5. **Syntax-Masked & Format-Safe Compression:** Code fences, JSON/XML, stacktraces, paths, and `tools[]` are `0%` mutated; `TRON` applies only to inbound homogeneous JSON arrays in `tool_result`; `AgentOCR` never rasterizes active source code.
6. **Never Starve Debugging + Zero-HTTP-400 Guard:** Stacktraces, failing tests, and Circuit-Breaker trips (`>= 2x` repeat with atomic pair collapse) unconditionally force `DEEP` reasoning; `validator.rs` verifies OpenAI/Anthropic message graphs pre-serialization and rolls back automatically on any violation.

---

## Documentation

* [`ARCHITECTURE.md`](./ARCHITECTURE.md) — Full `v0.8` specification, Mermaid diagrams, wire examples, and citations.
* [`BENCHMARKS.md`](./BENCHMARKS.md) — Full 9-stage ablation tables, Criterion latencies, and `--live-ab` instructions.
* [`AGENTS.md`](./AGENTS.md) — Engineering guardrails, `< 250` lines/file cap, and verification gates.
