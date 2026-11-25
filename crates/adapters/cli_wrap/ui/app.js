(() => {
  const { fmtTokens, fmtUsd, renderStackedChart } = window.AphelionCharts;
  const { render: renderTopo, pulse: pulseTopo } = window.AphelionTopology;
  const qs = new URLSearchParams(location.search);
  const tokQuery = qs.get("token") ? `?token=${encodeURIComponent(qs.get("token"))}` : "";
  const state = { rows: [], recent: [], sortKey: "req", sortAsc: false };

  const $ = id => document.getElementById(id);
  const esc = s => String(s ?? "").replace(/[&<>"']/g, c => (
    { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]
  ));
  const activePort = location.port || (location.protocol === "https:" ? "443" : "80");
  $("port-pill").textContent = `:${activePort}`;

  document.querySelectorAll("#nav-tabs .tab").forEach(btn => {
    btn.addEventListener("click", () => {
      document.querySelectorAll("#nav-tabs .tab").forEach(b => b.classList.remove("active"));
      document.querySelectorAll(".panel").forEach(p => p.classList.remove("active"));
      btn.classList.add("active");
      $("tab-" + btn.dataset.tab).classList.add("active");
    });
  });

  function renderKpis(k) {
    if (!k) return;
    $("kpi-req").textContent = String(k.total_requests || 0);
    $("kpi-hit").textContent = Number(k.verified_cache_hit_pct || 0).toFixed(1) + "%";
    $("kpi-saved").textContent = fmtUsd(k.total_net_saved_usd || 0);
    $("kpi-ttft").textContent = (k.avg_ttft_ms || 0) + "ms";
    $("kpi-in").textContent = `${fmtTokens(k.cached_read_tokens)} / ${fmtTokens(k.uncached_input_tokens)}`;
    $("kpi-out").textContent = `${fmtTokens(k.output_tokens)} (${fmtTokens(k.reasoning_tokens)})`;
  }

  function renderFeed() {
    const box = $("live-feed");
    if (!state.recent.length) {
      box.innerHTML = '<div class="empty">Waiting for live traffic...</div>';
      return;
    }
    box.innerHTML = state.recent.slice(0, 50).map(ev => {
      const d = new Date(ev.timestamp_ms || Date.now());
      const hms = d.toISOString().slice(11, 19);
      const sc = Number(ev.status_code || 200);
      const cls = sc >= 500 ? "red" : sc >= 400 ? "amber" : "green";
      const badges = (ev.badges || []).map(b => `<span class="badge">${esc(b)}</span>`).join("");
      return `<div class="feed-row mono">
        <span class="muted">${esc(hms)}</span>
        <span>${esc(ev.client || "CLI")}</span>
        <span>${esc(ev.provider)}/${esc(ev.model)}${badges}</span>
        <span class="${cls}">${sc}</span>
        <span>${Number(ev.ttft_ms || 0)}ms</span>
        <span class="green">${Number(ev.cache_hit_pct || 0).toFixed(1)}%</span>
        <span>${fmtTokens(ev.cached_in)}/${fmtTokens(ev.uncached_in)}</span>
        <span>${fmtTokens(ev.output)}(r:${fmtTokens(ev.reasoning)})</span>
        <span class="green">${fmtUsd(ev.net_saved_usd)}</span>
      </div>`;
    }).join("");
  }

  function renderTable() {
    const q = ($("pm-filter").value || "").toLowerCase();
    const list = state.rows
      .filter(r => !q || String(r.provider).toLowerCase().includes(q) || String(r.model).toLowerCase().includes(q))
      .sort((a, b) => {
        const av = a[state.sortKey], bv = b[state.sortKey];
        const cmp = typeof av === "string" ? av.localeCompare(bv) : (av || 0) - (bv || 0);
        return state.sortAsc ? cmp : -cmp;
      });
    $("pm-tbody").innerHTML = list.map(r => `<tr class="mono">
      <td>${esc(r.provider)}</td><td>${esc(r.model)}</td><td class="num">${Number(r.req || 0)}</td>
      <td class="num">${Number(r.avg_ttft_ms || 0)}ms</td><td class="num green">${Number(r.cache_hit_pct || 0).toFixed(1)}%</td>
      <td class="num">${fmtTokens(r.cached_in)} / ${fmtTokens(r.uncached_in)}</td>
      <td class="num">${fmtTokens(r.output)} (${fmtTokens(r.reasoning)})</td>
      <td class="num green">${fmtUsd(r.net_saved_usd)}</td>
    </tr>`).join("") || '<tr><td colspan="8" class="empty">No matching rows</td></tr>';
  }

  async function refresh() {
    const [ov, tp] = await Promise.all([
      fetch("/api/overview" + tokQuery).then(r => r.json()).catch(() => null),
      fetch("/api/topology" + tokQuery).then(r => r.json()).catch(() => null),
    ]);
    if (ov) {
      renderKpis(ov.kpis);
      state.rows = ov.by_provider_model || [];
      if (ov.recent && ov.recent.length && !state.recent.length) state.recent = ov.recent;
      if (ov.policy) {
        $("pol-output").value = ov.policy.output_mode || "concise";
        $("pol-compact").value = ov.policy.compaction_mode || "auto";
        $("pol-reason").value = ov.policy.reasoning_effort || "default";
      }
      renderTable();
      renderFeed();
      renderStackedChart($("token-chart"), ov.timeseries || []);
    }
    if (tp) {
      $("uptime-pill").textContent = `${tp.uptime_secs || 0}s`;
      renderTopo($("topo-svg"), tp);
    }
  }

  async function pushPolicy() {
    await fetch("/api/policy" + tokQuery, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        output_mode: $("pol-output").value,
        compaction_mode: $("pol-compact").value,
        reasoning_effort: $("pol-reason").value,
      }),
    });
    refresh();
  }
  ["pol-output", "pol-compact", "pol-reason"].forEach(id => $(id).addEventListener("change", pushPolicy));
  $("pm-filter").addEventListener("input", renderTable);
  document.querySelectorAll("th[data-sort]").forEach(th => {
    th.addEventListener("click", () => {
      const k = th.dataset.sort;
      state.sortAsc = state.sortKey === k ? !state.sortAsc : false;
      state.sortKey = k;
      renderTable();
    });
  });

  const base = `${location.origin}/v1`;
  const snippets = [
    ["OpenCode", `"provider": { "openai": { "options": { "baseURL": "${base}" } } }`],
    ["Claude Code", `aphelion --audit claude\n# or ANTHROPIC_BASE_URL=${location.origin}`],
    ["Cursor (via Tunnel)", `cloudflared tunnel --url ${location.origin}\n# Paste https://<tunnel>.trycloudflare.com/v1 in Cursor Models`],
    ["Cline / Roo Code", `API Provider: OpenAI Compatible\nBase URL: ${base}`],
  ];
  $("connect-grid").innerHTML = snippets.map(([name, code], i) => `<div class="card">
    <div class="card-head"><h2>${esc(name)}</h2><button class="copy-btn" data-copy="${i}">Copy</button></div>
    <pre class="snippet mono">${esc(code)}</pre>
  </div>`).join("");
  document.querySelectorAll("[data-copy]").forEach(btn => {
    btn.addEventListener("click", () => {
      navigator.clipboard?.writeText(snippets[Number(btn.dataset.copy)][1]);
      btn.textContent = "Copied!";
      setTimeout(() => (btn.textContent = "Copy"), 1200);
    });
  });

  const es = new EventSource("/api/events" + tokQuery);
  es.onopen = () => { $("sse-pill").classList.add("live"); $("sse-label").textContent = "Live"; };
  es.onerror = () => { $("sse-pill").classList.remove("live"); $("sse-label").textContent = "Reconnecting"; };
  es.addEventListener("request", e => {
    const ev = JSON.parse(e.data);
    state.recent.unshift(ev);
    if (state.recent.length > 50) state.recent.pop();
    renderFeed();
    pulseTopo(ev.client, ev.provider);
  });

  async function refreshAccountsAndCombos() {
    const [ac, cb, ig] = await Promise.all([
      fetch("/api/accounts" + tokQuery).then(r => r.json()).catch(() => null),
      fetch("/api/combos" + tokQuery).then(r => r.json()).catch(() => null),
      fetch("/api/integrations" + tokQuery).then(r => r.json()).catch(() => null),
    ]);
    if (ig && $("ide-activator-box")) {
      $("ide-activator-box").innerHTML = (ig.integrations || []).map(item => `
        <div class="feed-row mono" style="display:flex;justify-content:space-between;align-items:center;padding:10px 4px">
          <div>
            <strong>${esc(item.name)}</strong>
            <span class="${item.active ? "green" : "amber"}" style="margin-left:8px">[${item.active ? "ACTIVE" : "INACTIVE"}]</span>
            <div class="muted" style="font-size:12px">${esc(item.config_path)} | current: ${esc(item.current_url || "(default)")}</div>
          </div>
          <button class="copy-btn" data-ide-id="${esc(item.id)}" data-ide-act="${item.active ? "deactivate" : "activate"}">
            ${item.active ? "Deactivate & Restore" : "Activate Aphelion"}
          </button>
        </div>`).join("") || '<div class="empty">No supported IDEs detected</div>';
      document.querySelectorAll("[data-ide-id]").forEach(btn => {
        btn.addEventListener("click", async () => {
          btn.disabled = true;
          await fetch("/api/integrations" + tokQuery, {
            method: "POST", headers: { "content-type": "application/json" },
            body: JSON.stringify({ id: btn.dataset.ideId, action: btn.dataset.ideAct, proxy_base: location.origin }),
          });
          refreshAccountsAndCombos();
        });
      });
    }
    if (ac && $("ac-tbody")) {
      $("ac-tbody").innerHTML = (ac.accounts || []).map(({ account: a, quota: q }) => `<tr class="mono">
        <td>${esc(a.id)}</td><td>${esc(a.provider)}</td><td>${esc(a.dialect)}</td><td>${esc(a.tier)}</td>
        <td>${esc(a.secret_masked || "—")}</td><td class="${q.status === "healthy" ? "green" : "amber"}">${esc(q.status)}</td>
        <td class="num">${Number(q.requests_today || 0)}</td>
        <td class="num"><button class="copy-btn" data-del-acct="${esc(a.id)}">Remove</button></td>
      </tr>`).join("") || '<tr><td colspan="8" class="empty">No accounts configured</td></tr>';
      document.querySelectorAll("[data-del-acct]").forEach(btn => {
        btn.addEventListener("click", async () => {
          await fetch("/api/accounts/" + encodeURIComponent(btn.dataset.delAcct) + tokQuery, { method: "DELETE" });
          refreshAccountsAndCombos();
        });
      });
    }
    if (cb && $("cb-json") && document.activeElement !== $("cb-json")) {
      $("cb-json").value = JSON.stringify(cb, null, 2);
    }
  }
  if ($("ac-save")) {
    $("ac-save").addEventListener("click", async () => {
      await fetch("/api/accounts" + tokQuery, {
        method: "POST", headers: { "content-type": "application/json" },
        body: JSON.stringify({
          id: $("ac-id").value.trim(), provider: $("ac-provider").value.trim() || "custom",
          base_url: $("ac-url").value.trim(), secret: $("ac-key").value.trim(),
          dialect: $("ac-dialect").value, tier: $("ac-tier").value, enabled: true,
        }),
      });
      $("ac-key").value = "";
      refreshAccountsAndCombos();
    });
  }
  if ($("cb-save")) {
    $("cb-save").addEventListener("click", async () => {
      try {
        const parsed = JSON.parse($("cb-json").value);
        await fetch("/api/combos" + tokQuery, {
          method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(parsed),
        });
        $("cb-save").textContent = "Saved!";
        setTimeout(() => ($("cb-save").textContent = "Save Combos"), 1200);
      } catch { alert("Invalid JSON"); }
    });
  }

  refresh();
  refreshAccountsAndCombos();
  setInterval(refresh, 5000);
})();
