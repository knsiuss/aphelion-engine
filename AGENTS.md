# AGENTS.md — Engineering Rules & Architectural Guardrails

You are building **Aphelion (`aphelion`)**, a Sans-I/O Symbiotic token and inference optimization engine for coding agents and reasoning LLMs. Read `ARCHITECTURE.md` before modifying any module.

---

## 1. Non-Negotiable Hard Invariants

Every PR, module, or refactor MUST satisfy all six invariants:

1. **Zero Prefix Mutation:** Bytes at offsets `< frozen_prefix_len` or inside any `cache_control` breakpoint are strictly immutable. Never reorder, strip, or inject bytes inside a frozen prefix.
2. **Prefix-Safe Soft Tool-Gating Only:** Outbound `tools[]` schemas and descriptions must NEVER be truncated or deleted after Turn-0. Narrow the active tool space exclusively via RoPE tail soft-gating (`[Active Tool Scope: ... only; ignore all other tools]`). Suppress `@patch:@block` and assistant prefill during active tool-calling turns.
3. **BPE-Calibrated Cache Amortization:** Never use `bytes / 4` heuristics for `diet_gc` or `optical_history` thresholds. Use dialect-calibrated BPE token estimators (`crates/telemetry`), and compact only when stale savings exceed the amortization watermark (`4,000` / `6,000` tokens).
4. **Lifecycle Cold-Start Guard:** `ExecutionProfile::Ephemeral` (`cli_wrap`) MUST NEVER load ONNX Runtime sessions (`slm-onnx`). Use `LexicalEntropyScorer` (`< 2ms` CPU) for ephemeral invocations; reserve warm ONNX encoders for `ExecutionProfile::LongLived`.
5. **Syntax-Masked & Format-Safe Compression Only:**
   - `compressor` (`LLMLingua-2` / `LongLLMLingua`) must mask code fences, inline code, JSON/XML, stacktraces, and `tools[]` as `Protected` (`0%` mutation).
   - `tron_codec` transcodes **only inbound homogeneous JSON arrays** inside `tool_result` payloads; outbound `tool_calls` remain 100% standard JSON.
   - `optical_history` (`AgentOCR`) must **never** rasterize active source code files.
6. **Never Starve Debugging:** Stacktraces (`Error:`, `Traceback`, `panic:`), failing test outputs, or Circuit-Breaker trips (`>= 2x` identical action/error) unconditionally escalate complexity to `DEEP`, disabling thought-stream priming.

---

## 2. Anti-Slop Code Standards

* **Strict File Size Cap:** Keep every source file under **250 lines**. Split cohesive sub-responsibilities into dedicated modules rather than growing monolithic files.
* **Pure Sans-I/O Core (`crates/core`):** Zero `tokio`, `std::net`, `reqwest`, or filesystem I/O inside `crates/core`. All core transforms operate strictly on `&[u8]`, `Vec<u8>`, and `SessionState`.
* **Zero Panics on Hot Paths:** Never use `.unwrap()` or `.expect()` on untrusted network/JSON/SSE byte slices in `crates/core`. Return typed `CoreError` variants or fall back cleanly to `STATE_PASSTHROUGH`.
* **No Placeholder Logic:** Do not leave `TODO` stubs in state transitions or safety gates. Every fallback path must be deterministic and testable.

---

## 3. Safe-Mode Routing Matrix

| Request State | Prefix & `tools[]` | Prose / TRON / Optical | Diet GC & Economizer | `@patch:@block` & Priming |
| :--- | :--- | :--- | :--- | :--- |
| **Turn-0 (Init)** | Sort keys, prune impossible tool domains, lock `frozen_prefix_len` & Two-Phase ID | Compress static prose & Turn-0 RAG via lifecycle scorer | Initialize session state | Active if code `> 40` lines, non-tool, & `TRIVIAL` |
| **Turn-N (Normal)** | **100% Immutable** `< frozen_prefix_len` | LongLLMLingua + LLMLingua-2 on new RAG; TRON on `tool_result` | Watermark GC (`> 4k` tok) + Soft Tail Tool-Gating | Active only if non-tool & `TRIVIAL` |
| **Active Tool Loop** | **100% Intact** | TRON transcoding on inbound `tool_result` arrays | Soft Tail Tool-Gating + Micro-Skill + Compiled SLM check | **OFF** |
| **Failure Loop (`>= 2x`)** | **100% Immutable** `< frozen_prefix_len` | Masked (preserve full trace) | **Circuit Breaker ON:** Collapse retries + pivot Micro-Skill | **OFF** (`DEEP` forced) |

---

## 4. Verification & Testing Gates

Before considering any task complete, verify:
1. **Fragmentation Chaos (`tests/fuzz_sse.rs`):** `StreamFSM` produces byte-identical output when SSE streams are split into 1-byte chunks across `<think>`, UTF-8 multi-byte boundaries, and `@patch:@block` fences.
2. **Cache Invariant Tests:** Assert `out_bytes[..frozen_prefix_len] == prev_out_bytes[..frozen_prefix_len]` across multi-turn trajectories.
3. **Latency Budgets (`benches/core_latency.rs`):** `< 3ms` p99 total core transform overhead on 50 KB payloads in `ExecutionProfile::Ephemeral`.
