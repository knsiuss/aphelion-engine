# Aphelion

A local HTTP/SSE proxy and CLI wrapper for LLM coding agents, SDKs, and routers that reduces token spend without breaking upstream KV-cache prefixes.

Written in Rust. Ships as a single static binary (`aphelion`) and an npm launcher (`aphelion-engine`).

---

## Why

Most prompt compressors mutate earlier turns in `messages[]`. On providers with prefix caching (Anthropic, OpenAI, DeepSeek, Gemini), changing a single byte at message `k` invalidates the cached prefix from `k..N` and bills the entire suffix at full uncached input rates (`10x` cached reads).

Aphelion enforces **append-only prefix invariance**:

1. **Freeze once, replay verbatim:** On Turn 0, tool schemas and static prefix fields are canonicalized and locked in `PrefixLedger`. Historical turns `0..k` are replayed byte-for-byte on subsequent turns (`S_prefix = 1.00`), while user edits/rewinds fork cleanly instead of overwriting user changes.
2. **Optimize only new ingress:** File-read deduplication (`XXH3_64`), homogeneous JSON array compaction (`TRON`), and repetitive log collapsing run strictly on newly appended messages (`messages[k..n]` or Responses `input[k..n]`) before their first dispatch.
3. **Turn-0 output steering:** Output tokens cost `3x–5x` more than uncached input. Concise output steering is injected on Turn 0 (supporting top-level `system`, Anthropic `system[]` arrays, OpenAI `messages[0]` `system`/`developer`, Responses `instructions`, and Gemini `systemInstruction`) before the prefix freezes.
4. **Amortized macro-compaction:** Per-turn micro-GC is disabled. History compaction runs only when context exceeds the watermark (default `120,000` BPE tokens) and projected future cache-read savings beat the one-time re-prefill cost.
5. **Cache-drop circuit breaker:** Tracks actual upstream `cached_tokens` / `cache_read_input_tokens` / `prompt_cache_hit_tokens`. If rolling cache-hit drops below `70%` on turns `>= 3`, the session locks into pass-through prefix preservation.

---

## Install

```bash
# Run directly (macOS arm64/x64, Linux x64 musl, Windows x64)
npx aphelion-engine serve --port 18787

# Or install globally
npm i -g aphelion-engine
```

Prebuilt static binaries are also attached to [GitHub Releases](https://github.com/knsiuss/aphelion-engine/releases).

From source (requires Rust stable):

```bash
cargo build --release -p aphelion-cli
```

---

## Quickstart

### 1. CLI Wrapper (Claude Code, OpenCode, Aider, Codex)

Spawns an ephemeral local proxy on a free port, injects provider base-URL env vars (`OPENAI_BASE_URL`, `ANTHROPIC_BASE_URL`, `GEMINI_API_BASE_URL`, `OLLAMA_HOST`), and shuts down when the child process exits.

```bash
aphelion claude
aphelion --audit opencode
aphelion --audit --upstream http://127.0.0.1:20128/v1 claude   # chain to 9Router / LiteLLM
```

> **Note on Claude Code:** If `~/.claude/settings.json` defines `env.ANTHROPIC_BASE_URL`, Aphelion detects it and passes a temporary `--settings` override so traffic routes through the local proxy.

### 2. Gateway Mode + Local Dashboard (`/ui`)

```bash
aphelion serve --port 18787 --audit
```

Open the embedded dashboard or terminal monitor in a second shell:

```bash
aphelion ui --port 18787     # opens http://127.0.0.1:18787/ui
aphelion top                 # zero-dependency ANSI live monitor
aphelion doctor              # prints baseURL setup for detected local IDEs
```

Point any client or SDK to `http://127.0.0.1:18787/v1` (OpenAI-compatible) or `http://127.0.0.1:18787` (Anthropic/Gemini/Ollama):

```python
from openai import OpenAI
client = OpenAI(base_url="http://127.0.0.1:18787/v1")
```

> **Note on Cursor:** Cursor routes custom base URLs through its cloud backend (`api2.cursor.sh`), so `127.0.0.1` is unreachable. Expose your local port with `cloudflared tunnel --url http://127.0.0.1:18787` and use the resulting HTTPS URL + `/v1`.

### 3. Audit Reports & Live Metrics

`--audit` is optional. Without it, Aphelion runs strictly in RAM with zero disk writes. When enabled, `prefix_ledger` stores only cryptographic hashes and counters (never raw prompt or file text).

```bash
aphelion --audit-summary
aphelion --audit-summary --provider mistral --last 24h
aphelion --audit-export usage.jsonl
```

Gateway endpoints:
- `GET /health` — liveness JSON
- `GET /metrics` — Prometheus counters labeled by `provider`, `model`, `status_class`, and token `type`
- `GET /ui`, `GET /api/overview`, `GET /api/topology`, `GET /api/events` (SSE), `POST /api/policy`

---

## Auto-Detect Routing

`AutoRouter` resolves the upstream without config files using 4 stages:

1. **Explicit override:** `X-Aphelion-Upstream` header (with dynamic hostname labeling, e.g. `https://api.moonshot.cn/v1` -> `moonshot.cn`) or path prefix (`/openai`, `/anthropic`, `/gemini`, `/openrouter`, `/deepseek`, `/mistral`, `/moonshot`, `/dashscope`, `/zhipu`, `/groq`, `/xai`, `/fireworks`, `/huggingface`, `/cerebras`, `/perplexity`, `/together`, `/github`, `/ollama`).
2. **Unambiguous API keys (in-memory only, never logged):** `sk-or-v1-`, `sk-ant-`, `sk-proj-`, `sk-svcacct-`, `AIza`, `gsk_`, `xai-`, `fw_`, `hf_`, `csk-`, `pplx-`, `tgp_v1_`, `ghp_`, `github_pat_`.
3. **Wire dialect & model disambiguation:** Resolves generic `sk-` collisions before falling back to OpenAI — `/v1/messages` -> Anthropic, `deepseek-*` -> DeepSeek, `kimi-*`/`moonshot-*` -> Moonshot, `qwen*`/`qwq-*` -> DashScope, `glm-*` -> Zhipu, `mistral-*`/`codestral-*`/`devstral-*` -> Mistral, `<name>:<tag>` -> Ollama.
4. **Non-inference passthrough:** `/v1/models`, `/v1/embeddings`, `/v1/messages/count_tokens`, `/v1/audio/*`, `/v1/images/*`, `/v1/files`, and `X-Aphelion-Bypass: true` pass through untouched.

All defaults can be overridden via CLI flags or `APHELION_*` environment variables (`APHELION_PORT`, `APHELION_BIND`, `APHELION_RPM`, `APHELION_MAX_BODY_MB`, `APHELION_AUDIT_DB`, `APHELION_ADMIN_TOKEN`, `APHELION_UPSTREAM_<PROVIDER>`, `APHELION_COMPACTION_THRESHOLD`, `APHELION_TAIL_MESSAGES`).

---

## Public BYOK Mode

```bash
aphelion serve --public --bind 0.0.0.0:18787 --rpm 120 --max-body-mb 10
```

In `--public` mode:
- Requests without a credential header (`Authorization`, `x-api-key`, `x-goog-api-key`, `api-key`) receive `401`.
- Per-tenant sliding-window rate limiting enforces `429` with `Retry-After`.
- SSRF guard blocks loopback, RFC1918, link-local, IPv4-mapped IPv6, `@` path authority injection, redirects, and `/ollama`.
- Gzip bodies are strictly capped at `max_body_bytes` during decompression (`413` on overflow).
- `/ui*` and `/api/*` return `404` unless `--admin-token <secret>` is configured.

---

## Benchmarks ([BENCHMARKS.md](./BENCHMARKS.md))

| Fixture | Raw BPE | Opt BPE | Payload Saved | KV-Cache Recovery | `S_prefix` | Tools Parity |
| :--- | ---: | ---: | ---: | ---: | ---: | :---: |
| `coding_multi_turn.jsonl` (12 turns) | 228,199 | 41,999 | 81.9% (\$0.5586) | 0.4% (\$0.0024) | 1.00 | 100% |
| `mcp_tool_heavy.jsonl` (25 tools + TRON) | 7,191 | 6,960 | 5.0% (\$0.0007) | 36.3% (\$0.0078) | 1.00 | 100% |
| `rag_long_context.jsonl` (RAG + code mask) | 1,748 | 1,255 | 28.2% (\$0.0015) | 0.0% (\$0.0000) | 1.00 | 100% |

CPU overhead (`ExecutionProfile::Ephemeral`, Criterion): `266 µs` (10 KB) | `1.35 ms` median / `1.38 ms` p99 (50 KB) | `5.51 ms` (200 KB).

---

## Documentation

- [ARCHITECTURE.md](./ARCHITECTURE.md) — Sans-I/O pipeline design and wire invariants
- [BENCHMARKS.md](./BENCHMARKS.md) — 9-stage ablation tables and Criterion latency numbers
- [AGENTS.md](./AGENTS.md) — Hard invariants and `< 250` lines/file engineering rules

MIT License.
