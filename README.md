# Aphelion (`aphelion`) v1.0

**Universal Sans-I/O Layer-3 AI Inference & Token Optimizer for Every Coding Agent, Router, SDK, and Non-Agentic Workload.**

`aphelion` transparently optimizes OpenAI Chat, OpenAI Responses (`/v1/responses`), Anthropic Messages (`/v1/messages`), Google Gemini (`*:generateContent`), and Ollama (`/api/chat`, `/api/generate`) traffic using **Strict KV-Cache Preservation (Append-Only Prefix Invariance)**:

- **`PrefixLedger` & Append-Only History Lock:** Freezes Turn-0 `system` + deterministically sorted `tools[]` and replays historical messages `0..k` byte-for-byte identically on every subsequent turn ($S_{\text{prefix}} = 1.00$), preventing mid-history cache-busting regressions.
- **Pre-Commit Ingress Optimization ("Optimize Once, Freeze Forever"):** Deduplicates identical repeated `read_file` calls (by `XXH3_64` hash), applies `TRON` tabular compression, and collapses repetitive log lines on newly appended messages `messages[k..n]` **before** first dispatch.
- **Cache-Aware Telemetry & Cache-Drop Circuit Breaker:** Tracks real upstream `uncached_input_tokens` vs. `cached_read_tokens` (`prompt_tokens_details.cached_tokens`, `cache_read_input_tokens`, `prompt_cache_hit_tokens`) and automatically locks into `FrozenPrefixPass` if rolling `cache_hit_ratio < 0.70` on turns `>= 3`.
- **Automatic Workload Routing:** Separates **agentic loops**, **direct code edits**, **single-turn RAG/chat**, **structured JSON outputs**, and **transparent passthrough** (`/v1/models`, `/v1/embeddings`, `X-Aphelion-Bypass: true`) without ever injecting agentic directives into non-agentic requests.

---

## 30-Second Quickstarts

### 1. Ephemeral CLI Wrapper (Windows `.exe`/`.cmd`/`.ps1` & POSIX)
Zero daemon. Automatically resolves `opencode.cmd`, `claude.ps1`, `aider.exe`, etc., injects all provider base URLs, and prints net token/cache savings on exit:

```bash
aphelion --audit opencode
aphelion --audit claude
aphelion --audit --upstream http://localhost:20128/v1 claude   # chain to 9Router
aphelion --audit-summary                                       # verified cache hits & $ saved
```

### 2. Universal Gateway Mode (`aphelion serve`)
Start a single local gateway for GUI IDEs, routers, SDKs, Chat UIs, and `curl`:

```bash
aphelion serve --port 18787 --upstream https://api.openai.com/v1 --audit
```

* **OpenCode (`opencode.json`) / Cursor / Cline / Roo Code:** Point your OpenAI/Anthropic provider `baseURL` to `http://127.0.0.1:18787/v1` (or `http://127.0.0.1:18787/anthropic`).
* **Python `openai` SDK (non-agentic RAG / structured JSON):**
  ```python
  from openai import OpenAI
  client = OpenAI(base_url="http://127.0.0.1:18787/v1")
  ```
* **Multi-Provider Path & Header Routing:**
  - `/openai/...`, `/anthropic/...`, `/gemini/...`, `/ollama/...`
  - Or pass `X-Aphelion-Upstream: https://custom-router.local/v1` per request (and `X-Aphelion-Bypass: true` to skip all transforms).
* **Raw `curl`:**
  ```bash
  curl http://127.0.0.1:18787/v1/chat/completions -H "Authorization: Bearer $OPENAI_API_KEY" \
    -H "Content-Type: application/json" -d '{"model":"gpt-4o","messages":[{"role":"user","content":"Hello"}]}'
  ```

### 3. In-Process Router Middleware (`@aphelion/middleware`)

```js
import { createAphelionMiddleware } from "@aphelion/middleware";
const aphelion = createAphelionMiddleware({ profile: "LongLived" });
```

---

## Empirical Benchmark Summary ([BENCHMARKS.md](./BENCHMARKS.md))

| Fixture | Raw BPE Tok | Opt BPE Tok | Payload Saved | KV-Cache Recovery | $S_{\text{prefix}}$ | Tools Parity |
| :--- | ---: | ---: | ---: | ---: | ---: | :---: |
| `coding_multi_turn.jsonl` (12 turns, Active Pinning) | 228,199 | 41,999 | **81.9%** (\$0.5586) | **0.4%** (\$0.0024) | **1.00** | **100%** |
| `mcp_tool_heavy.jsonl` (25 tools + TRON + 3x loop) | 7,191 | 6,960 | **5.0%** (\$0.0007) | **36.3%** (\$0.0078) | **1.00** | **100%** |
| `rag_long_context.jsonl` (RAG + code mask) | 1,748 | 1,255 | **28.2%** (\$0.0015) | 0.0% (\$0.0000) | **1.00** | **100%** |

- **Criterion CPU Latency (`ExecutionProfile::Ephemeral`):** `266 µs` (10 KB) | **`1.35 ms` median / `1.38 ms` p99 CI (50 KB, `< 3 ms` PASS)** | `5.51 ms` (200 KB).

---

## Documentation

* [`ARCHITECTURE.md`](./ARCHITECTURE.md) — Full `v0.8` specification, Mermaid diagrams, and research citations.
* [`BENCHMARKS.md`](./BENCHMARKS.md) — Split-attribution 9-stage ablation tables, Criterion latencies, and `--live-ab`.
* [`AGENTS.md`](./AGENTS.md) — The 6 Non-Negotiable Hard Invariants and `< 250` lines/file engineering guardrails.
