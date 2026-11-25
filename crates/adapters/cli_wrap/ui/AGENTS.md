# AGENTS.md — Aphelion Embedded Web UI (`crates/adapters/cli_wrap/ui`)

This directory contains the zero-dependency embedded dashboard served directly by the `aphelion` Rust binary at `/ui`.

---

## 0. STRICT SCOPE LOCK — FRONTEND ONLY (DO NOT TOUCH BACKEND)

- **YOU MAY ONLY EDIT** the 5 files inside `crates/adapters/cli_wrap/ui/`:
  - `index.html`, `styles.css`, `app.js`, `topology.js`, `charts.js`
- **NEVER MODIFY ANY RUST OR CONFIG FILES** outside this folder (`*.rs`, `Cargo.toml`, `Cargo.lock`, `tests/`, `crates/core/`).
- If a desired UI feature needs a new backend field/endpoint that is not listed in Section 2, **STOP and ask the user** — do not edit `ui_api.rs` or any Rust code yourself.

---

## 1. Architecture & Build Constraints

1. **Embedded via `include_str!`:**
   - [ui_api.rs](../src/ui_api.rs) embeds **only** these 5 files at compile time:
     - `index.html` -> `GET /ui`
     - `styles.css` -> `GET /ui/styles.css`
     - `app.js` -> `GET /ui/app.js`
     - `topology.js` -> `GET /ui/topology.js`
     - `charts.js` -> `GET /ui/charts.js`
   - **No build step / no npm / no bundler:** Keep everything pure HTML5, modern CSS, and vanilla ES2020+ JS.
   - Any new `.js` or `.css` file will **NOT** be served unless explicitly added to `include_str!` and the match router in `ui_api.rs` (prefer editing the existing 5 files).
2. **Strict Content-Security-Policy (CSP):**
   - Served with: `default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:`
   - **NO inline `<script>` tags**, **NO `onclick="..."` HTML attributes**, **NO external CDNs** (Google Fonts, Tailwind CDN, unpkg, etc.).
   - Attach all event listeners in `app.js` via `addEventListener`.
3. **XSS & Secret Safety:**
   - Always escape dynamic strings with `esc(...)` before inserting into `innerHTML`.
   - Never store or display plaintext API keys in `localStorage` or DOM text (API returns `secret_masked`).
   - Preserve `?token=...` (`tokQuery`) on every `fetch()` and `EventSource()` call so `--public` admin token auth keeps working.
4. **Rebuild Required to Preview Changes:**
   - Because assets are compiled into the binary, after editing `ui/*`, rebuild/reinstall:
     ```powershell
     cargo build -p aphelion-cli
     # or global install:
     cargo install --path crates\adapters\cli_wrap --force
     ```

---

## 2. Backend REST & SSE API Contracts

All endpoints live under the same origin (`/api/...`). Append `tokQuery` (`?token=...`) to every request.

| Endpoint | Method | Purpose & Payload |
| :--- | :--- | :--- |
| `/api/overview` | `GET` | `{ kpis, by_provider_model, recent, timeseries, policy }` (`policy` includes `adaptive_fallback: bool` and `fallback_models: string[]`) |
| `/api/topology` | `GET` | `{ uptime_secs, clients, upstreams, engine }` for SVG graph |
| `/api/events` | `GET` | `EventSource` SSE (`event: request`, JSON `TelemetryEvent`) |
| `/api/policy` | `POST` | `{ output_mode?, compaction_mode?, reasoning_effort?, adaptive_fallback?, fallback_models? }` |
| `/api/accounts` | `GET` | `{ accounts: [{ account: { id, label, provider, base_url, dialect, tier, secret_masked, enabled, daily_budget_usd }, quota: { status, cooldown_remaining_secs, requests_today, tokens_today, cost_today_usd } }] }` |
| `/api/accounts` | `POST` | Upsert `ProviderAccount` JSON (`id`, `provider`, `base_url`, `secret`, `dialect`: `"openai" \| "anthropic"`, `tier`: `"free" \| "subscription" \| "cheap" \| "paid"`, `enabled`: `true`) |
| `/api/accounts/:id` | `DELETE` | Remove account by ID |
| `/api/combos` | `GET` | `{ combos: { "<name>": [{ model, account_id?, upstream?, keys? }] }, strategies: { "<name>": "failover" \| "flash_first" \| "round_robin" } }` |
| `/api/combos` | `POST` | Replace `ComboConfig` JSON |
| `/api/integrations` | `GET` | `{ proxy_base, integrations: [{ id, name, detected, active, config_path, current_url, can_restore }] }` |
| `/api/integrations` | `POST` | `{ id: "claude_code", action: "activate" \| "deactivate", proxy_base: location.origin }` |

---

## 3. UI Quality & UX Goals

- Keep all 6 tabs functional: **Topology**, **Accounts**, **Combos**, **Analytics**, **Providers**, **Quick Connect** (includes 1-Click IDE Activation).
- Do not break existing DOM IDs expected by `app.js` unless updating both `index.html` and `app.js` together.
- Verify responsive layout (desktop + narrow split-pane) and verify `cargo test -p aphelion-cli --test ui_and_router_v32` passes after changes.
