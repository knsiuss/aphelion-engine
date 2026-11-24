# Aphelion (`aphelion`) — Symbiotic Token & Inference Optimizer (`ARCHITECTURE.md` v0.8)

## 1. Design Philosophy & Core Thesis

Conventional token optimization fails when applied blindly: naive lexical compression destroys upstream KV-caches and corrupts code/JSON syntax, while adversarial prompt constraints (*"think briefly"*) trigger **analysis paralysis** in reasoning models.

This system is engineered as a **Symbiotic Cognitive & Trajectory Prosthetic** built on a **Sans-I/O (Pure Transform Core)** architecture. It unifies hardware inference alignment, runtime trajectory garbage collection, lifecycle-aware prompt compression, tabular payload transcoding, optical history tiering, and compiled SLM routing:

1. **Zero-I/O Core:** All transformation logic operates strictly on byte slices and an incremental streaming finite-state machine (FSM) with zero network dependencies. The same core compiles into an ephemeral CLI wrapper (zero background daemon), a router middleware plugin (9Router / LiteLLM), or an in-process WASM/C-ABI SDK.
2. **Hardware & Neural Alignment:** Every transformation maps directly to internal inference engine mechanics—RadixAttention / PagedAttention KV-caching, RoPE attention windows, ViT visual patch caches, BPE subword tokenization, and RL stopping policies.
3. **Multi-Modal & Multi-Tier Context Compression:**
   - **Prose & RAG (`LLMLingua-2` & `LongLLMLingua`):** `4x–20x` token-density pruning and U-shaped salience reordering on unstructured natural-language RAG passages, automatically adapting its scoring backend to runtime lifecycle constraints (`< 2ms` lexical scorer for ephemeral CLI vs. warm INT8 ONNX encoder for long-lived runtimes).
   - **Structured Tool Payloads (`TRON / TOON`):** `18%–27%` token savings by transcoding repetitive JSON arrays in `tool_result` responses into compact tabular notation.
   - **Visual History (`AgentOCR`):** `> 50%` token savings on VLM backends by rasterizing archived terminal/DOM observation logs into cacheable monochrome image tiles.
4. **BPE-Calibrated Trajectory Diet (`AgentDiet`):** Deterministically collapses superseded file reads and resolved error steps—governed by a **model-calibrated KV-Cache Amortization Watermark** so compaction never triggers unprofitable cache rewrites.
5. **Prefix-Safe Soft Tool Governance & Compiled Workflow Routing (`TokenEconomizer` & `Workflow Compilation`):** Suppresses tool-space logit dilution via RoPE tail soft-gating without mutating frozen `tools[]` prefixes, trips a deterministic **Circuit Breaker** on repeating failure loops, and routes recurring stabilized sub-workflows to local compiled SLMs (`up to 100x` cost reduction).

---

## 2. End-to-End System Architecture

```mermaid
flowchart LR
    subgraph Adapters [Thin Runtime Adapters - Lifecycle Tagged]
        CLI[Ephemeral CLI Wrapper\nProfile: Ephemeral | Random Port]
        RouterPlugin[Router Middleware\nProfile: LongLived | 9Router / LiteLLM]
        SDK[In-Process SDK\nProfile: LongLived | WASM / C-ABI]
    end

    subgraph Core [Pure Transform Core - Zero Network I/O]
        direction TB
        N[1. Dialect Normalizer & Two-Phase Session Guard\nOpenAI / Anthropic + cache_control + Workspace Hash]
        C[2. Turn-0 Canonicalizer & BPE-Exact Diet GC\nPrefix Freeze + Calibrated Watermark + AgentOCR]
        P[3. Lifecycle-Aware Prose Compressor & TRON Codec\nLexical / Warm-ONNX RAG + Tabular Tool-Result]
        E[4. Economizer & Compiled-SLM Router\nSoft Tail Tool-Gating + Loop Breaker + SLM Dispatch]
        V[5. CPU AST & Fact Verifier\nTree-sitter @block + Complexity Gate < 5ms]
        S[6. Symbiotic Composer\nSafe-Mode Matrix + RoPE Tail-Steer + Prefill]
        FSM[7. Streaming Byte FSM\nThink Distiller + Loop Tracker + AST Patcher]

        N --> C --> P --> E --> V --> S
    end

    Adapters -->|Raw Request Bytes + ExecutionProfile| N
    E -.->|Compiled Sub-Workflow| LocalSLM[(Local Compiled SLM\n0.5B-3B Weights)]
    S -->|Optimized Request Bytes| Upstream[(Upstream Router / Frontier API / vLLM)]
    Upstream -->|Raw SSE Byte Chunks| FSM
    FSM -->|Reconstructed SSE + Metrics| Adapters
```

---

## 3. Core Sub-Systems & Under-the-Hood Mechanics

### 3.1. Dialect Normalizer & Two-Phase Session Guard (`core/normalizer`)
* **Role:** Maps raw JSON payloads from OpenAI (`/v1/chat/completions`) and Anthropic (`/v1/messages`) dialects into a zero-copy internal intermediate representation (IR) without altering underlying message bytes.
* **Explicit Breakpoint Detection (`cache_control` Awareness):** Scans for provider cache directives (e.g., Anthropic/Bedrock `cache_control: {"type": "ephemeral"}`). All bytes from offset `0` through the final explicit `cache_control` breakpoint are marked as a **Sacred Read-Only Zone**—zero bytes may be reordered, pruned, compressed, or injected within this span.
* **Collision-Proof Two-Phase Session Derivation:**
  * *Why naive hashing fails:* Hashing only `system_prompt || first_user_msg` causes session collisions when developers launch multiple concurrent sessions with identical system prompts and generic opening prompts (e.g., *"Fix the failing tests in this repo"*).
  * *Phase 1 (Turn-0 Provisional Key):* Extracts workspace/repository root identifiers (`cwd`, workspace path tags, or container IDs present in initial environment blocks) and computes:
    `session_id = XXH3_128(canonical(system_prompt) || workspace_fingerprint || first_user_msg[0..512])`
  * *Phase 2 (Turn-1+ Turn-Bound Alias):* Once the first assistant turn completes, binds `XXH3_64(first_assistant_msg[0..256])` (capturing unique initial `tool_call_id`s and initial generation bytes) as a secondary lookup key in `SessionState`, guaranteeing zero cross-session state bleed even across identical directories.

### 3.2. Turn-0 Prefix Canonicalizer, BPE-Exact Diet GC & Optical History (`core/canonicalizer`, `core/diet_gc`, `core/optical_history`)
* **Under-the-Hood Target:** **PagedAttention** / **RadixAttention** fixed-block prefix caching vs. quadratic $O(N^2)$ trajectory bloat (**AgentDiet** & **AgentOCR**).
* **Turn-0 Prefix Freeze:**
  * On Turn-0 (`session_init`), lexicographically sorts JSON keys across static metadata and `tools[]`, relocates volatile tokens (ISO timestamps, UUIDs, dynamic git state) to the user message tail, and executes initial Turn-0 static prose compression before locking `frozen_prefix_len` in `SessionState`.
  * On Turn-N, offsets `< frozen_prefix_len` are strictly immutable.
* **Model-Calibrated Watermark Trajectory Pruning (`diet_gc`):**
  * *Avoiding Heuristic Drift:* Naive `bytes / 4` estimators drift by `25%–30%` on symbol-dense source code across provider tokenizers (`o200k_base`, Claude BPE, SentencePiece), risking premature compaction that burns `1.25x` cache-write premiums. `diet_gc` computes exact token counts via a dialect-specific `TokenEstimator` (`tiktoken-rs` for `o200k_base`/`cl100k_base` and calibrated BPE byte-pair lookup tables for Claude/Gemini/Qwen).
  * Continuously tracks **Redundant Trajectory Bloat ($B_{\text{stale}}$)** past `frozen_prefix_len`:
    1. **Superseded File Reads:** A `read_file` output at turn $t_1$ for path $P$ where $P$ was subsequently modified at turn $t_2 > t_1$ $\rightarrow$ collapsed into a deterministic tombstone (`[AgentDiet Tombstone: auth.ts read at turn #2; superseded by edit at turn #5]`).
    2. **Resolved Error Loops:** Stacktraces or failing test logs at turn $t_1$ followed by a passing test run at turn $t_2 > t_1$ $\rightarrow$ collapsed into `[Resolved: test failure at turn #3 fixed at turn #6]`.
    3. **Unreferenced Search Outputs:** `grep` / `list_dir` outputs with zero symbol/path overlap in subsequent turns.
  * Compaction executes **only when** exact $B_{\text{stale}}$ exceeds the **Cache Amortization Threshold** (default: `4,000` model-calibrated tokens), guaranteeing multi-turn cache-read savings exceed the one-time `1.25x` cache-rewrite cost.
* **Optical Observation History (`optical_history` — AgentOCR Integration):**
  * When `diet_gc` still leaves `> 6,000` calibrated tokens of non-code historical observations (verbose terminal logs, CI matrices, browser DOM snapshots) and the target model supports vision (`supports_vision == true`), archived observation turns are rasterized on CPU via an embedded bitmap font renderer (`fontdue`, zero browser dependency) into high-contrast `1024x1024` monospace PNG tiles (`> 50%` token reduction vs. raw text).
  * **Source-Code Precision Guardrail:** Active source code files are **never** rasterized (avoiding ViT `14x14` patch ambiguity on punctuation/indentation) and always remain as AST-anchored text (`@block`).

### 3.3. Lifecycle-Aware Syntax-Masked Prose Compressor & TRON Codec (`core/compressor` & `core/tron_codec`)
* **Under-the-Hood Target:** Natural-language entropy (`LLMLingua-2`), RoPE **"Lost in the Middle"** decay (`LongLLMLingua`), and JSON structural token overhead (`TRON / TOON`).
* **Three-Stage Prose & RAG Compression (`core/compressor`):**
  1. **Syntax & Structural Masking Pass:** Partitions input into `Protected` vs. `Compressible` spans. All fenced code blocks (` ```...``` `), inline code, JSON/XML payloads, stacktraces, file paths, URLs, and `tools[]` schemas are marked `Protected` (`0%` mutation).
  2. **LongLLMLingua U-Shaped Relevance Reordering:** Scores multi-chunk RAG documents against the active user query and arranges them along a **U-shaped salience curve** (highest-relevance chunks at the leading and trailing edges of the RAG block; lowest-relevance chunks in the middle).
  3. **Lifecycle-Aware LLMLingua-2 Pruning (`4x–20x`):** Selects the `TokenScorer` backend based on the adapter's `ExecutionProfile` to eliminate cold-start latency spikes:
     * `ExecutionProfile::Ephemeral` (`cli_wrap`): Strictly uses the zero-weight `LexicalEntropyScorer` (`< 2ms` pure CPU, avoiding the `80–200ms` ONNX Runtime session initialization overhead on short-lived CLI invocations).
     * `ExecutionProfile::LongLived` (`RouterPlugin` / in-process `SDK`): Preloads an INT8-quantized **BERT / XLM-RoBERTa token-classification encoder** (`--features slm-onnx`) once into warm memory (`< 15ms` inference per 4k tokens) for high-precision semantic pruning.
* **Prefix-Safe TRON / TOON Transcoder (`core/tron_codec`):**
  * **Inbound Tool-Result Compaction (`18%–27%` token savings):** Detects homogeneous JSON arrays (`>= 3` objects sharing identical keys) inside `tool_result` / MCP payloads and transcodes them into tabular TRON records (`@tron[id,path,line]\n1|src/auth.ts|42`) with a 1-line schema header.
  * **Outbound Schema & Logit Protection:** Outbound `tools[]` definitions and model-emitted `tool_calls` remain 100% standard JSON so native function-calling parsers and RL-trained tool logits are never disrupted.

### 3.4. Economizer Governance Gate & Compiled Workflow Router (`core/economizer` & `crates/distill`)
* **Under-the-Hood Target:** Action-space logit dilution, degenerate retry loops (**TokenEconomizer**), and frontier-model overuse on deterministic routines (**Workflow Compilation**).
* **Prefix-Safe Soft Tail Tool-Gating (Zero Schema Mutation):**
  * *Why hard deletion fails:* Because `tools[]` is locked inside the Turn-0 frozen prefix, deleting unused tool definitions mid-session invalidates the entire downstream KV-cache.
  * *Two-Stage Gating:* At **Turn-0**, deterministic workspace heuristics prune provably impossible tool domains *before* `frozen_prefix_len` locks. At **Turn-N**, `tools[]` bytes remain **100% untouched**, while a high-salience soft-gating directive is injected at the tail of the active user message (`[Active Tool Scope: read_file, apply_diff only; ignore all other tools]`). Leveraging RoPE recency bias, this suppresses logit dilution across 20+ MCP tools without invalidating a single prefix byte.
* **Deterministic Micro-Skill Injection:** Maps AST patterns, file extensions, or error signatures to compact procedural **Micro-Skills** (`20–50` tokens) injected transiently at the prompt tail and evicted on the next turn.
* **Failure-Loop Circuit Breaker:** Tracks `action_sig = XXH3_64(tool_name || canonical_args)` and `error_sig = XXH3_64(normalized_stderr_head)`. Identical repeats $\ge 2\times$ trigger **Trajectory Collapse** and inject a **Strategy-Pivot Directive** at the tail.
* **Compiled SLM Sub-Workflow Routing (`crates/distill`):**
  * *Offline Compilation Flywheel:* The `--audit` Flight Recorder logs verified `(workflow_sig, input, verified_output)` tuples from successful runs. Recurring deterministic sub-workflows (e.g., test-log summarization, commit/PR formatting, boilerplate migrations) are exported as LoRA/SFT datasets to compile lightweight local models (`0.5B–3B` weights).
  * *Confidence-Gated Dispatch:* When an incoming step matches a registered compiled workflow signature, `economizer` routes it to the local SLM endpoint (`up to 100x` cheaper than frontier APIs) and validates output syntax/schema before returning—falling back transparently to the frontier model on any validation failure.

### 3.5. CPU AST Verifier & Complexity Gate (`core/verifier`)
* **Under-the-Hood Target:** **BPE Subword Tokenization** weakness in counting line numbers/whitespace vs. LLM strength in symbolic anchoring (`< 5ms` CPU budget).
* Disables `@patch:@block` output contracts and `Thought-Stream Priming` during active tool-calling turns (`tools[]` active and `tool_choice != "none"`).
* On direct code-generation turns (`> 40` lines), parses fenced blocks via **Tree-sitter**, injects semantic landmarks (`// @block:<symbol>#<hash4>`), records byte spans in `SessionState`, and injects verified caller counts.
* Stacktraces or circuit-breaker trips unconditionally force complexity to `DEEP`, disabling thought priming.

### 3.6. Symbiotic Composer & Safe-Mode Routing Matrix (`core/composer`)

| Request State | Prefix & `tools[]` | Prose, TRON & Optical Tiers | Trajectory Diet & Economizer | `@patch:@block` & Priming |
| :--- | :--- | :--- | :--- | :--- |
| **Turn-0 (Session Init)** | Sort keys, prune impossible tool domains, lock `frozen_prefix_len` & Two-Phase Session ID | Compress static system prose & Turn-0 RAG (`4x–20x`) via lifecycle-appropriate scorer | Baseline state initialized | Active if code `> 40` lines, non-tool, & `TRIVIAL` |
| **Turn-N (Normal Progression)** | **100% Immutable** `< frozen_prefix_len` | LongLLMLingua + LLMLingua-2 on new RAG; TRON on tabular `tool_result` | BPE-calibrated Watermark GC (`> 4k` tok) + Soft Tail Tool-Gating | Active only if non-tool & `TRIVIAL` |
| **Active Tool-Calling Loop** | **100% Intact** (zero `tools[]` mutation) | TRON transcoding on inbound `tool_result` arrays | Soft Tail Tool-Gating (`only; ignore all other tools`) + Micro-Skill + SLM check | **OFF** (protects tool-call JSON logits) |
| **Failure Loop ($\ge 2\times$ Repeat)** | **100% Immutable** `< frozen_prefix_len` | Masked (preserve full error trace fidelity) | **Circuit Breaker ON:** Collapse duplicate retries + inject pivot Micro-Skill | **OFF** (`DEEP` reasoning forced) |

### 3.7. Streaming Byte FSM & AST Reconstructor (`core/stream_fsm` & `core/patcher`)
1. `STATE_THINK`: Maintains a rolling 320-byte tail window inside `<think>` blocks; extracts the terminal 1–2 conclusion sentences upon hitting `</think>` for `Self-Handoff Memory`.
2. `STATE_SNIFF`: Inspects the first 64 bytes of visible output (bypassed immediately on tool-call deltas). Transitions to `STATE_PATCH` if prefixed with `@patch:@block:`; otherwise transitions immediately to `STATE_PASSTHROUGH` with zero added latency.
3. `STATE_PATCH`: Buffers the updated block, splices it into the cached AST byte span in memory, validates syntactic integrity via Tree-sitter, and emits standard SSE events containing the complete reconstructed file.

---

## 4. Wire-Level Payload Transformation (RAG + TRON MCP Output + Soft Tail Tool-Gating)

```json
// BEFORE (Turn-4): Verbose JSON array from MCP tool, unordered RAG, stale file read
{
  "model": "claude-sonnet-4-20250514",
  "tools": [ /* 24 MCP tools */ ],
  "messages": [
    { "role": "system", "content": "Coding agent. Time: 2026-11-24T15:53:00Z" },
    { "role": "user", "content": "Audit failing auth endpoints in /workspace/auth-service using these docs:\n[Doc 1: Overview... 1,200 tok]\n[Doc 2: Exact JWT kid rotation spec... 900 tok]" },
    { "role": "assistant", "tool_calls": [{ "id": "1", "function": { "name": "read_file", "arguments": "{\"path\":\"src/jwt.ts\"}" }}] },
    { "role": "tool", "tool_call_id": "1", "content": "/* 400 lines of old src/jwt.ts */" },
    { "role": "assistant", "tool_calls": [{ "id": "2", "function": { "name": "search_endpoints", "arguments": "{\"service\":\"auth\"}" }}] },
    { "role": "tool", "tool_call_id": "2", "content": "[{\"id\":\"ep_1\",\"method\":\"POST\",\"path\":\"/v1/login\",\"auth\":\"jwt\"},{\"id\":\"ep_2\",\"method\":\"POST\",\"path\":\"/v1/refresh\",\"auth\":\"jwt\"},{\"id\":\"ep_3\",\"method\":\"GET\",\"path\":\"/v1/me\",\"auth\":\"jwt\"}]" }
  ]
}
```

```json
// AFTER (Turn-4): Frozen prefix + U-reordered compressed RAG + AgentDiet tombstone + TRON + Soft Tool-Gating
{
  "model": "claude-sonnet-4-20250514",
  "tools": [ /* 24 MCP tools - byte-identical to Turn-0 for 100% KV-cache hit */ ],
  "messages": [
    { "role": "system", "content": "Coding agent." },
    {
      "role": "user",
      "content": "Audit failing auth endpoints in /workspace/auth-service using these docs:\n[Doc 2 (High Salience, Compressed 4.3x): JWT rotation requires kid header check + 300s clock skew leeway... 210 tok]\n[Doc 1 (Compressed 6.6x): Auth overview... 180 tok]"
    },
    { "role": "assistant", "tool_calls": [{ "id": "1", "function": { "name": "read_file", "arguments": "{\"path\":\"src/jwt.ts\"}" }}] },
    { "role": "tool", "tool_call_id": "1", "content": "[AgentDiet Tombstone: src/jwt.ts (400 lines) superseded by subsequent edits]" },
    { "role": "assistant", "tool_calls": [{ "id": "2", "function": { "name": "search_endpoints", "arguments": "{\"service\":\"auth\"}" }}] },
    {
      "role": "tool",
      "tool_call_id": "2",
      "content": "@tron[id,method,path,auth]\nep_1|POST|/v1/login|jwt\nep_2|POST|/v1/refresh|jwt\nep_3|GET|/v1/me|jwt\n\n[Active Tool Scope: edit_file, run_test only; ignore all other tools]"
    }
  ]
}
```

---

## 5. Falsifiable Proof & Benchmark Architecture

### 5.1. Net Token, Cache-Amortized & Compiled-SLM Economics
Telemetry computes **Effective Cost ($C_{\text{eff}}$)** across frontier cache tiers and local compiled SLM dispatches:

$$C_{\text{eff}} = (T_{\text{in\_uncached}} \cdot P_{\text{in}}) + (T_{\text{cache\_write}} \cdot P_{\text{write}}) + (T_{\text{cache\_read}} \cdot P_{\text{read}}) + ((T_{\text{think}} + T_{\text{out}}) \cdot P_{\text{out}}) + (T_{\text{slm\_local}} \cdot P_{\text{slm}})$$

* **Prose & RAG Compression ($\eta_{\text{prose}}$):** `4x–20x` reduction on masked natural-language RAG spans with `0%` mutation on protected code/JSON spans.
* **TRON Payload Savings ($\eta_{\text{tron}}$):** `18%–27%` token reduction on tabular MCP/`tool_result` JSON payloads.
* **Trajectory & Optical Diet ($\eta_{\text{diet}}$):** `45%–60%+` cumulative input token reduction across multi-turn coding and VLM trajectories.
* **Compiled Workflow Savings ($\eta_{\text{compile}}$):** Up to `100x` cost reduction on repetitive sub-workflows routed to distilled local SLMs.

### 5.2. Behavioral & Analysis-Paralysis Metrics
* **Reasoning-to-Action Ratio ($\rho_{\text{RA}} = T_{\text{think}} / T_{\text{code\_patch}}$):** Confirms suppression of overthinking on `TRIVIAL` edits while preserving unbounded reasoning on `DEEP` debugging.
* **Self-Doubt & Backtrack Density ($\delta_{\text{hedge}}$):** Lexical frequency of hesitation pivots (*"Wait,"*, *"Hold on,"*, *"Actually,"*, *"Let me double check"*) per 1,000 `<think>` tokens.
* **Code Bloat Delta ($\Delta_{\text{AST}}$):** AST node count and cyclomatic complexity delta against minimal ground-truth patches.

### 5.3. Shadow A/B Replay Harness & 9-Stage Ablation Matrix

```mermaid
flowchart TD
    Trace[(Recorded Agent, MCP & VLM Trajectories\n.jsonl)] --> Splitter{A/B Benchmark Runner}
    
    Splitter -->|Path A: Control| Raw[Raw Unmodified Trajectory]
    Splitter -->|Path B: Treatment| Core[Symbiotic Pure Core]
    
    Raw --> LLM1[Upstream Frontier / Local vLLM]
    Core --> LLM2[Upstream Frontier / Compiled SLM]
    
    LLM1 --> Eval1[SWE-bench / LongBench + Tool Accuracy\n+ Cache Hit & Net $ Recorder]
    LLM2 --> Eval2[SWE-bench / LongBench + Tool Accuracy\n+ Cache Hit & Net $ Recorder]
    
    Eval1 --> DiffReport[Falsification Report\nPass@1 Delta | Tool Parity | Input Diet % | Net $ Saved]
    Eval2 --> DiffReport
```

* **Acceptance Criteria:**
  * Functional equivalence: $\text{Pass@1}_{\text{Symbiotic}} \ge \text{Pass@1}_{\text{Raw}} - 1.0\%$.
  * Tool-calling parity: `0%` regression in tool selection accuracy and JSON argument schema validity.
  * Syntactic integrity: `100%` of reconstructed files parse via Tree-sitter with zero `ERROR` nodes.
* **Automated 9-Stage Ablation:** Evaluates `Baseline`, `+PrefixLock`, `+AgentDiet_GC`, `+LLMLingua_Prose`, `+TRON_Codec`, `+AgentOCR_History`, `+TokenEconomizer_Gates`, `+AST_Block_Priming`, and `Full Symbiotic (+CompiledSLM)` independently.

### 5.4. White-Box GPU Inference Probe & Chaos Fuzzing
* **vLLM / SGLang Prometheus Probe:** Measures `vllm:gpu_prefix_cache_hit_rate` and `vllm:time_to_first_token_seconds` to verify GPU KV-block reuse across text and vision patch tokens.
* **1-Byte SSE Fragmentation Fuzzer (`tests/fuzz_sse.rs`):** Asserts byte-for-byte reconstruction equivalence under 1-byte and random chunk splits across `<think>` blocks, tool-call deltas, and UTF-8 boundaries.
* **CPU Microbenchmarks (`benches/core_latency.rs`):** Enforces `< 3ms` p99 overhead in `ExecutionProfile::Ephemeral` and `< 18ms` p99 in `ExecutionProfile::LongLived` (warm ONNX).
* **Audit Flight Recorder (`--audit`):** Logs every AgentDiet tombstone, TRON table conversion, AgentOCR tile hash, Micro-Skill injection, Circuit Breaker trip, and compiled-SLM route to local SQLite.

---

## 6. Hard Invariants

1. **Zero Prefix Mutation:** Bytes at offsets `< frozen_prefix_len` or inside any `cache_control` breakpoint are strictly immutable.
2. **Prefix-Safe Soft Tool-Gating Only:** Outbound `tools[]` schemas are never deleted or mutated mid-session; tool-space narrowing occurs exclusively via RoPE tail soft-gating (`[Active Tool Scope: ... only; ignore all other tools]`), and diff/priming injections are suppressed during active tool-calling turns.
3. **BPE-Calibrated Cache Amortization:** Trajectory pruning and optical rasterization execute only when model-calibrated BPE stale token savings exceed watermark thresholds (`4,000` / `6,000` tokens), never via `bytes / 4` heuristics.
4. **Lifecycle Cold-Start Guard:** `ExecutionProfile::Ephemeral` (`cli_wrap`) never initializes ONNX Runtime sessions, guaranteeing `< 10ms` total CLI wrapper overhead.
5. **Syntax-Masked & Format-Safe Compression Only:** `LLMLingua-2` never touches code fences or schemas; `TRON` transcoding applies only to inbound tabular `tool_result` payloads; `AgentOCR` never rasterizes active source code files.
6. **Never Starve Debugging:** Stacktraces, Circuit-Breaker trips, or failed compiled-SLM schema checks unconditionally escalate to `DEEP` frontier reasoning.

---

## 7. Modular Repository Structure

```text
aphelion/
├── crates/
│   ├── core/                     # Sans-I/O Pure Transform Engine (<250 lines/file)
│   │   ├── normalizer.rs         # OpenAI/Anthropic mapper, cache_control & Two-Phase Session ID
│   │   ├── canonicalizer.rs      # Turn-0 key sort + Turn-N byte freeze + XXH3 hash
│   │   ├── diet_gc.rs            # BPE-calibrated watermark trajectory pruning (AgentDiet)
│   │   ├── optical_history.rs    # CPU bitmap rasterizer for VLM log history (AgentOCR)
│   │   ├── compressor.rs         # Lifecycle-aware LLMLingua-2 (Lexical / Warm-ONNX) & LongLLMLingua
│   │   ├── tron_codec.rs         # Inbound tabular JSON -> TRON/TOON transcoder
│   │   ├── economizer.rs         # Soft tail tool-gating, micro-skills, loop breaker & SLM route
│   │   ├── verifier.rs           # Tree-sitter @block tagger + Tool-Mode & DEEP gates
│   │   ├── composer.rs           # Safe-Mode matrix + RoPE tail-steer + prefill
│   │   ├── stream_fsm.rs         # Incremental SSE scanner & rolling distiller
│   │   └── patcher.rs            # AST byte-span replacer & fallback validator
│   ├── telemetry/                # Dialect BPE tokenizers (tiktoken-rs + lookup tables) & cost ledger
│   ├── distill/                  # Flight-Recorder trajectory miner & LoRA/SFT exporter
│   └── adapters/
│       ├── cli_wrap/             # Ephemeral wrapper (Profile::Ephemeral, random port, zero daemon)
│       └── ffi_wasm/             # WASM / C-ABI exports (Profile::LongLived) for 9Router / SDKs
├── bench/
│   ├── harness/                  # Shadow A/B runner + 9-stage ablation generator
│   └── vllm_probe/               # Prometheus GPU KV-cache & TTFT verifier
├── tests/
│   └── fuzz_sse.rs               # 1-byte fragmentation & UTF-8 boundary chaos tests
└── ARCHITECTURE.md
```

---

## 8. Citations & Architectural Lineage

| Architectural Component | Reference (Paper / Project / Specification) | Adopted Technical Principle |
| :--- | :--- | :--- |
| **Inbound Tabular Payload Codec (`tron_codec`)** | **TRON & TOON (2026)**, *A Benchmark Study of Token-Optimized Formats in Agentic AI*. | Replacing repetitive JSON punctuation and duplicated keys in structured MCP/tool payloads with compact tabular record notation reduces tokens by 18%–27% without breaking deterministic parsing. |
| **Optical Observation History (`optical_history`)** | **AgentOCR (ACL 2026)**, *Reimagining Agent History via Optical Self-Compression*. | Rendering verbose historical agent observations into compressed monospace images leverages ViT visual token density and visual KV-caching to cut tokens by >50% while retaining >95% text performance. |
| **Offline Trajectory Distillation & SLM Routing (`distill` & `economizer`)** | **Workflow Compilation (2026)**, *Compiling Agentic Workflows into LLM Weights*. | Distilling stabilized, repetitive agent sub-workflows into dedicated small model weights reduces execution cost by up to two orders of magnitude (100x) compared to frontier models. |
| **Lifecycle-Aware Prose & RAG Compressor (`compressor`)** | **Jiang et al. (EMNLP 2023)**, *LLMLingua*; **Pan et al. (ACL 2024)**, *LLMLingua-2*; **Jiang et al. (ACL 2024)**, *LongLLMLingua* (Microsoft Research). | Quantized encoder token-classification prunes low-information prose (`4x–20x`), while query-aware U-shaped chunk reordering counteracts "lost in the middle" decay; lifecycle gating avoids ONNX cold-start overhead in ephemeral CLIs. |
| **BPE-Calibrated Watermark Trajectory Diet (`diet_gc`)** | **AgentDiet (2026)**, *Runtime Trajectory Pruning for Efficient Coding Agents*; **OpenAI `tiktoken` / BPE Tokenizers**. | Deterministic runtime tombstoning of superseded file reads and resolved errors cuts up to 60% of input tokens; model-calibrated BPE counting prevents heuristic threshold drift on symbol-dense code. |
| **Soft Tail Tool-Gating, Micro-Skills & Loop Circuit Breaker (`economizer`)** | **Microsoft Research (2026)**, *TokenEconomizer: Dynamic Action-Space Governance, Micro-Skill Injection, and Failure-Loop Termination in Autonomous Agents*. | Suppressing irrelevant tools via RoPE tail soft-gating preserves frozen `tools[]` KV-caches while preventing logit dilution, micro-skill injection, and repetitive error loops. |
| **Turn-0 Prefix-Lock & `cache_control` Guard** | **Kwon et al. (SOSP 2023)**, *PagedAttention* (vLLM); **Zheng et al. (NeurIPS 2024)**, *RadixAttention* (SGLang); **Anthropic Prompt Caching Spec**. | Fixed-block prefix-tree KV-cache matching; mutating bytes inside an established prefix or `cache_control` span invalidates downstream cached blocks, requiring Turn-0 freeze and watermark-amortized compaction. |
| **Tail-Steer Injection & Append-Only History** | **Liu et al. (TACL 2024)**, *Lost in the Middle*; **Su et al. (2021)**, *RoFormer (RoPE)*; **Xiao et al. (ICLR 2024)**, *StreamingLLM (Attention Sinks)*. | U-shaped attention distribution across long contexts: placing active tool scopes, micro-skills, and output contracts at the prompt tail maximizes adherence without mutating cached prefixes. |
| **Thought-Stream Priming & Pre-Satisfied Verification** | **DeepSeek-AI (2025)**, *DeepSeek-R1* (GRPO); **Chen et al. (2024)**, *Do NOT Think That Much for 2+3=? On the Overthinking of o1-Like LLMs*; **Anthropic / vLLM Prefill Spec**. | RL length bias induces self-doubt loops and autoregressive self-conditioning; upfront deterministic facts and assistant prefill collapse redundant verification branches on trivial tasks. |
| **AST `@block` Landmarks & Delta Reconstruction** | **Paul Gauthier (Aider)**, *AST Repo-Map & Diff Edit Benchmarks*; **Max Brunsfeld**, *Tree-sitter*; **Sennrich et al. (ACL 2016)**, *Byte-Pair Encoding (BPE)*. | BPE merges indentation irregularly and splits line numbers across tokens; AST-anchored symbolic IDs eliminate manual line/whitespace counting by the model. |
| **Self-Handoff Memory** | **Shinn et al. (NeurIPS 2023)**, *Reflexion*; **Packer et al. (2023)**, *MemGPT*. | Carrying forward distilled self-authored conclusions across turns prevents reasoning amnesia when intermediate `<think>` traces are stripped. |
| **Sans-I/O Core & Ephemeral Wrapper** | **Cory Benfield**, *Building Protocol Libraries The Right Way (Sans-I/O)*; **9Router / LiteLLM** Reverse-Proxy Conventions. | Strict decoupling of byte-level protocol transformation from socket I/O enables zero-daemon CLI wrapping, router plugins, and WASM embedding from a single core. |
