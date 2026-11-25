(() => {
  const { fmtTokens, fmtUsd, renderStackedChart } = window.AphelionCharts;
  const { render: renderTopo, pulse: pulseTopo } = window.AphelionTopology;
  const qs = new URLSearchParams(location.search);
  const tokQuery = qs.get("token") ? `?token=${encodeURIComponent(qs.get("token"))}` : "";
  const state = { rows: [], recent: [], sortKey: "req", sortAsc: false };

  const $ = id => document.getElementById(id);
  $("port-pill").textContent = `:${location.port || 18787}`;

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
      const sc = ev.status_code || 200;
      const cls = sc >= 500 ? "red" : sc >= 400 ? "amber" : "green";
      const badges = (ev.badges || []).map(b => `<span class="badge">${b}</span>`).join("");
      return `<div class="feed-row mono">
        <span class="muted">${hms}</span>
        <span>${ev.client || "CLI"}</span>
        <span>${ev.provider}/${ev.model}${badges}</span>
        <span class="${cls}">${sc}</span>
        <span>${ev.ttft_ms}ms</span>
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
      .filter(r => !q || r.provider.toLowerCase().includes(q) || r.model.toLowerCase().includes(q))
      .sort((a, b) => {
        const av = a[state.sortKey], bv = b[state.sortKey];
        const cmp = typeof av === "string" ? av.localeCompare(bv) : (av || 0) - (bv || 0);
        return state.sortAsc ? cmp : -cmp;
      });
    $("pm-tbody").innerHTML = list.map(r => `<tr class="mono">
      <td>${r.provider}</td><td>${r.model}</td><td class="num">${r.req}</td>
      <td class="num">${r.avg_ttft_ms}ms</td><td class="num green">${Number(r.cache_hit_pct || 0).toFixed(1)}%</td>
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
    ["Cursor", `Settings -> Models -> Override OpenAI Base URL:\n${base}`],
    ["Cline / Roo Code", `API Provider: OpenAI Compatible\nBase URL: ${base}`],
  ];
  $("connect-grid").innerHTML = snippets.map(([name, code], i) => `<div class="card">
    <div class="card-head"><h2>${name}</h2><button class="copy-btn" data-copy="${i}">Copy</button></div>
    <pre class="snippet mono">${code}</pre>
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
    refresh();
  });

  refresh();
  setInterval(refresh, 5000);
})();
