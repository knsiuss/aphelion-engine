(() => {
  const C = window.AphelionCharts;
  const { fmtTokens, fmtUsd, providerOf, pairLabel, sparkline, ledgerRow, providerRow, drawerHTML, isCached } = C;
  const topo = window.AphelionTopology;
  const qs = new URLSearchParams(location.search);
  const tokQuery = qs.get("token") ? `?token=${encodeURIComponent(qs.get("token"))}` : "";
  const S = {
    rows: [], recent: [], view: [], sortKey: "req", sortAsc: false,
    seen: false, gone: false, live: false, paused: false, filter: "all", q: "", peak: 1, tok: [], ttft: [],
  };
  const $ = id => document.getElementById(id);
  const esc = s => String(s ?? "").replace(/[&<>"']/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  const api = (p, init) => fetch(p + tokQuery, init);
  const post = (p, body) => api(p, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) }).catch(() => {});
  const port = location.port || (location.protocol === "https:" ? "443" : "80");
  $("port-pill").textContent = `:${port}`;
  const tabs = [...document.querySelectorAll("#nav-tabs .tab")];
  const select = name => {
    tabs.forEach(b => b.setAttribute("aria-selected", String(b.dataset.tab === name)));
    document.querySelectorAll(".panel").forEach(p => p.classList.toggle("active", p.id === `tab-${name}`));
  };
  tabs.forEach(btn => btn.addEventListener("click", () => select(btn.dataset.tab)));
  const link = (state, label) => { $("sse-pill").className = `pill ${state}`, $("sse-label").textContent = label; };
  function ledger(k) {
    if (!k) return;
    const hit = Number(k.verified_cache_hit_pct || 0).toFixed(1) + "%", val = $("kpi-hit");
    if (val.textContent !== hit) { val.textContent = hit; val.classList.remove("tick"); void val.offsetWidth; val.classList.add("tick"); }
    const net = Number(k.total_net_saved_usd || 0), saved = $("kpi-saved");
    saved.textContent = fmtUsd(net);
    saved.classList.toggle("zero", Math.abs(net) < 0.00005), saved.classList.toggle("loss", net < -0.00005);
    const cells = {
      "kpi-in": fmtTokens(k.cached_read_tokens), "kpi-uncached": fmtTokens(k.uncached_input_tokens),
      "kpi-out": fmtTokens(k.output_tokens), "kpi-reason": fmtTokens(k.reasoning_tokens),
      "kpi-req": String(k.total_requests || 0), "kpi-ttft": `${k.avg_ttft_ms || 0}ms`,
      "kpi-compactions": String(k.compactions_count || 0),
    };
    for (const id in cells) $(id).textContent = cells[id];
    const parts = [["cached", k.cached_read_tokens], ["uncached", k.uncached_input_tokens],
      ["output", k.output_tokens], ["reason", k.reasoning_tokens]].map(([n, v]) => [n, Number(v || 0)]);
    const total = parts.reduce((a, [, v]) => a + v, 0) || 1;
    $("split-bar").innerHTML = parts.filter(([, v]) => v > 0).map(([n, v]) => {
      const share = (v / total) * 100;
      return `<i class="seg-${n}" data-tip="${n} ${v.toLocaleString()} tokens · ${share.toFixed(1)}% of this session" style="width:${share}%"></i>`;
    }).join("");
  }
  // Tooltip is fed from the same numbers the bar is drawn from: tokens and share, no prices,
  // because the payload carries no per-token rate.
  const tip = $("tip");
  document.addEventListener("mousemove", e => {
    const seg = e.target.closest("#split-bar i");
    if (!seg) return tip.hidden = true;
    tip.textContent = seg.dataset.tip, tip.hidden = false;
    tip.style.left = `${Math.min(e.clientX + 12, innerWidth - 260)}px`, tip.style.top = `${e.clientY + 16}px`;
  });
  function applyFilters() {
    const q = S.q.toLowerCase(), now = Date.now();
    S.view = S.recent.filter(ev => {
      const st = Number(ev.status_code || 200);
      if (S.filter === "errors" && st < 400) return false;
      if (S.filter === "cached" && !isCached(ev)) return false;
      if (S.filter === "inflight" && now - Number(ev.timestamp_ms || 0) > 5000) return false;
      return !q || `${ev.client} ${ev.provider} ${ev.model}`.toLowerCase().includes(q);
    });
    S.peak = Math.max(1, ...S.view.map(ev => Number(ev.cached_in || 0) + Number(ev.uncached_in || 0)));
  }
  function feed(fresh) {
    const box = $("live-feed");
    if (S.gone) {
      box.innerHTML = `<div class="state">Dashboard API unreachable on <b>:${port}</b> — gateway stopped, or this origin refused.`
        + ` <button class="btn" id="retry">Retry now</button></div>`;
      $("retry").addEventListener("click", refresh);
      return;
    }
    if (!S.seen) return void (box.innerHTML = '<div class="skel" style="width:72%"></div><div class="skel" style="width:88%"></div><div class="skel" style="width:64%"></div>');
    if (!S.recent.length) {
      box.innerHTML = `<div class="state">Nothing has routed through this gateway yet, so there is no ledger to read.
        <br />Point a client at <b>${esc(location.origin)}/v1</b> — <b class="cta" data-goto="connect">Quick Connect</b> wires an IDE in one click.</div>`;
      box.querySelector("[data-goto]").addEventListener("click", () => select("connect"));
      return;
    }
    applyFilters();
    box.innerHTML = S.view.length
      ? S.view.slice(0, 50).map((ev, i) => ledgerRow(ev, S.peak, i)).join("")
      : `<div class="state">No row matches <b>${esc(S.filter)}${S.q ? ` + “${esc(S.q)}”` : ""}</b>.`,
    $("feed-meta").textContent = `${S.view.length}/${S.recent.length} rows · ${topo.concurrency()} in flight`;
    box.querySelectorAll(".row").forEach(r => {
      const open = () => openDrawer(Number(r.dataset.idx));
      r.addEventListener("click", open);
      r.addEventListener("keydown", e => { if (e.key === "Enter" || e.key === " ") open(); });
    });
  }
  function openDrawer(i) {
    const ev = S.view[i];
    if (!ev) return;
    const peak = Math.max(1, ...S.view.map(x => Number(x.ttft_ms || 0)));
    $("drawer").innerHTML = drawerHTML(ev, peak);
    $("drawer").classList.add("open"), $("scrim").classList.add("open");
    $("dw-close").addEventListener("click", closeDrawer);
  }
  const closeDrawer = () => { $("drawer").classList.remove("open"), $("scrim").classList.remove("open"); };
  $("scrim").addEventListener("click", closeDrawer);
  document.addEventListener("keydown", e => { if (e.key === "Escape") closeDrawer(); });
  document.querySelectorAll("#feed-filters .pill-btn").forEach(b => b.addEventListener("click", () => {
    S.filter = b.dataset.filter;
    document.querySelectorAll("#feed-filters .pill-btn").forEach(x => x.setAttribute("aria-pressed", String(x === b)));
    feed(false);
  }));
  $("feed-q").addEventListener("input", e => { S.q = e.target.value.trim(); feed(false); });
  // Pause freezes the animation and the row churn; the numbers keep polling.
  $("stream-toggle").addEventListener("click", () => {
    S.paused = !S.paused;
    topo.pause(S.paused);
    $("stream-toggle").textContent = S.paused ? "Resume" : "Pause";
    $("stream-toggle").setAttribute("aria-pressed", String(S.paused));
  });
  function table() {
    const q = ($("pm-filter").value || "").toLowerCase();
    const list = S.rows.filter(r => !q || String(r.provider).toLowerCase().includes(q) || String(r.model).toLowerCase().includes(q))
      .sort((a, b) => (S.sortAsc ? 1 : -1) * (typeof a[S.sortKey] === "string"
        ? String(a[S.sortKey]).localeCompare(String(b[S.sortKey])) : (a[S.sortKey] || 0) - (b[S.sortKey] || 0)));
    $("pm-tbody").innerHTML = list.map(providerRow).join("")
      || '<tr><td colspan="8" class="state">No provider has been routed to yet.</td></tr>';
  }
  async function refresh() {
    const [ov, tp] = await Promise.all([
      api("/api/overview").then(r => r.json()).catch(() => null),
      api("/api/topology").then(r => r.json()).catch(() => null),
    ]);
    S.gone = !ov && !tp;
    if (!S.live && (ov || tp)) link("wait", "Idle · no event yet");
    if (ov) {
      S.seen = true, ledger(ov.kpis), S.rows = ov.by_provider_model || [];
      if (ov.recent && ov.recent.length && !S.recent.length) {
        S.recent = ov.recent;
        S.tok = S.recent.slice(0, 40).reverse().map(ev => Number(ev.cached_in || 0) + Number(ev.output || 0));
        S.ttft = S.recent.slice(0, 40).reverse().map(ev => Number(ev.ttft_ms || 0));
        sparkline($("spark-tok"), S.tok, "tok"), sparkline($("spark-ttft"), S.ttft, "ttft");
      }
      if (ov.policy) {
        $("pol-output").value = ov.policy.output_mode || "concise";
        $("pol-compact").value = ov.policy.compaction_mode || "auto";
        $("pol-reason").value = ov.policy.reasoning_effort || "default";
      }
      table(), C.renderStackedChart($("token-chart"), ov.timeseries || []);
    }
    if (tp) {
      $("uptime-pill").textContent = `${tp.uptime_secs || 0}s`;
      $("kpi-sessions").textContent = String((tp.engine || {}).active_sessions || 0);
      topo.render($("topo-svg"), tp, S.rows, S.recent); // the graph owns the empty-state note
    }
    if (S.recent.length) S.seen = true;
    feed(false);
  }
  async function pushPolicy() {
    const [output_mode, compaction_mode, reasoning_effort] = ["pol-output", "pol-compact", "pol-reason"].map(id => $(id).value);
    await post("/api/policy", { output_mode, compaction_mode, reasoning_effort });
    refresh();
  }
  ["pol-output", "pol-compact", "pol-reason"].forEach(id => $(id).addEventListener("change", pushPolicy));
  $("pm-filter").addEventListener("input", table);
  document.querySelectorAll("th[data-sort]").forEach(th => th.addEventListener("click", () => {
    const k = th.dataset.sort;
    S.sortAsc = S.sortKey === k ? !S.sortAsc : false, S.sortKey = k, table();
  }));
  const base = `${location.origin}/v1`;
  const snippets = [
    ["OpenCode", `"provider": { "openai": { "options": { "baseURL": "${base}" } } }`],
    ["Claude Code", `aphelion --audit claude\n# or ANTHROPIC_BASE_URL=${location.origin}`],
    ["Cursor (via tunnel)", `cloudflared tunnel --url ${location.origin}\n# paste https://<tunnel>.trycloudflare.com/v1 into Cursor models`],
    ["Cline / Roo Code", `API Provider: OpenAI Compatible\nBase URL: ${base}`],
  ];
  $("connect-grid").innerHTML = snippets.map(([name, code], i) => `<section>
    <div class="inst-head"><h2>${esc(name)}</h2><button class="btn btn-quiet" data-copy="${i}">Copy</button></div>
    <pre class="snippet">${esc(code)}</pre></section>`).join("");
  document.querySelectorAll("[data-copy]").forEach(btn => btn.addEventListener("click", () => {
    navigator.clipboard?.writeText(snippets[Number(btn.dataset.copy)][1]);
    btn.textContent = "Copied", setTimeout(() => (btn.textContent = "Copy"), 1200);
  }));
  const es = new EventSource("/api/events" + tokQuery);
  es.onopen = () => { S.live = true; link("live", "Live"); };
  es.onerror = () => link("off", "Offline");
  es.addEventListener("request", e => {
    const ev = JSON.parse(e.data);
    if (!S.paused) {
      S.recent.unshift(ev);
      if (S.recent.length > 50) S.recent.pop();
      S.tok = [...S.tok, Number(ev.cached_in || 0) + Number(ev.output || 0)].slice(-60);
      S.ttft = [...S.ttft, Number(ev.ttft_ms || 0)].slice(-60);
      sparkline($("spark-tok"), S.tok, "tok"), sparkline($("spark-ttft"), S.ttft, "ttft");
    }
    S.seen = true, S.live = true, link("live", "Live");
    topo.pulse(ev);
    $("topo-empty").hidden = true;
    if (!S.paused) feed(true);
  });
  async function settings() {
    const [ac, cb, ig] = await Promise.all([
      api("/api/accounts").then(r => r.json()).catch(() => null),
      api("/api/combos").then(r => r.json()).catch(() => null),
      api("/api/integrations").then(r => r.json()).catch(() => null),
    ]);
    if (ig) {
      $("ide-activator-box").innerHTML = (ig.integrations || []).map(item => `<div class="ide-row"><div>`
        + `<strong>${esc(item.name)}</strong> <span class="tag">${item.active ? "active" : item.detected ? "detected" : "not found"}</span>`
        + `<div class="ide-meta">${esc(item.config_path)} · ${esc(item.current_url || "default endpoint")}</div></div>`
        + `<button class="btn ${item.active || !item.detected ? "btn-quiet" : "btn-primary"}" data-ide-id="${esc(item.id)}" data-ide-act="${item.active ? "deactivate" : "activate"}"${item.detected ? "" : " disabled"}>${item.active ? "Deactivate & restore" : "Activate Aphelion"}</button></div>`).join("")
        || '<div class="state">No supported IDE config was found on this machine.</div>';
      document.querySelectorAll("[data-ide-id]").forEach(btn => btn.addEventListener("click", async () => {
        btn.disabled = true;
        await post("/api/integrations", { id: btn.dataset.ideId, action: btn.dataset.ideAct, proxy_base: location.origin });
        settings();
      }));
    }
    if (ac) {
      $("ac-tbody").innerHTML = (ac.accounts || []).map(({ account: a, quota: q }) => `<tr><td class="mono">${esc(a.id)}</td>`
        + `<td>${esc(a.provider)}</td><td>${esc(a.dialect)}</td><td>${esc(a.tier)}</td><td class="mono quiet">${esc(a.secret_masked || "—")}</td>`
        + `<td class="${q.status === "healthy" ? "green" : "amber"}">${esc(q.status)}</td>`
        + `<td class="num mono">${Number(q.requests_today || 0)}</td><td class="num mono">${fmtUsd(q.cost_today_usd || 0)}</td>`
        + `<td class="num"><button class="btn btn-quiet" data-del-acct="${esc(a.id)}">Remove</button></td></tr>`).join("")
        || '<tr><td colspan="9" class="state">No account yet. Add one above — the gateway cannot route without a credential.</td></tr>';
      document.querySelectorAll("[data-del-acct]").forEach(btn => btn.addEventListener("click", async () => {
        await api("/api/accounts/" + encodeURIComponent(btn.dataset.delAcct), { method: "DELETE" }).catch(() => {});
        settings();
      }));
    }
    if (cb && document.activeElement !== $("cb-json")) $("cb-json").value = JSON.stringify(cb, null, 2);
  }
  $("ac-save").addEventListener("click", async () => {
    const val = id => $(id).value.trim();
    await post("/api/accounts", {
      id: val("ac-id"), provider: val("ac-provider") || "custom", base_url: val("ac-url"), secret: val("ac-key"),
      dialect: $("ac-dialect").value, tier: $("ac-tier").value, enabled: true,
    });
    $("ac-key").value = "";
    settings();
  });
  $("cb-save").addEventListener("click", async () => {
    const label = t => { $("cb-save").textContent = t; setTimeout(() => ($("cb-save").textContent = "Save combos"), 1400); };
    try { await post("/api/combos", JSON.parse($("cb-json").value)); label("Saved"); }
    catch { label("Invalid JSON"); }
  });
  link("wait", "Connecting");
  feed(false);
  refresh();
  settings();
  setInterval(refresh, 5000);
  setInterval(settings, 15000);
  setInterval(() => { if (!S.paused && S.view.length) $("feed-meta").textContent = `${S.view.length}/${S.recent.length} rows · ${topo.concurrency()} in flight`; }, 300);
})();