window.AphelionCharts = (() => {
  const esc = s => String(s ?? "").replace(/[&<>"']/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  const mono = v => String(v == null ? "" : v), fit = (s, m) => (s.length > m ? s.slice(0, m - 1) + "…" : s);
  const fmtTokens = n => (n >= 1e6 ? (n / 1e6).toFixed(1) + "M" : n >= 1e3 ? (n / 1e3).toFixed(1) + "k" : String(n || 0));
  const fmtUsd = v => `${Number(v || 0) < 0 ? "-$" : "$"}${Math.abs(Number(v || 0)).toFixed(4)}`;
  const signOf = v => (v > 0.00005 ? "green" : v < -0.00005 ? "amber" : "quiet");
  // The gateway labels a provider "unknown" when a route is resolved by combo/account rather
  // than by host, but the model id still carries its vendor prefix (mistralai/…, baidu/…).
  function providerOf(provider, model) {
    const p = String(provider || "").trim(), m = String(model || "");
    if (p && p !== "unknown") return p;
    const i = m.indexOf("/");
    return i > 0 ? m.slice(0, i) : "unknown";
  }
  const pairLabel = (provider, model) => {
    const m = String(model || ""), p = providerOf(provider, m);
    return p === "unknown" || p === m || m.startsWith(p + "/") ? m : `${p}/${m}`;
  };
  // Structured ids only: telemetry sometimes carries a prompt or terminal echo in the model
  // field, and those must never become a node or a table row.
  const MODEL_RE = /^[A-Za-z0-9][A-Za-z0-9._:@/-]{1,79}$/;
  const cleanModel = (provider, model) => {
    const m = String(model || "").trim();
    return MODEL_RE.test(m) && m !== provider ? m : null;
  };
  function renderStackedChart(canvas, buckets) {
    if (!canvas) return;
    const ctx = canvas.getContext("2d"), w = canvas.width, h = canvas.height;
    ctx.clearRect(0, 0, w, h);
    if (!buckets || !buckets.length) {
      ctx.fillStyle = "#6d6d78", ctx.font = "12px ui-monospace, monospace", ctx.textAlign = "center";
      return void ctx.fillText("No bucket recorded in this window yet", w / 2, h / 2);
    }
    const padL = 56, padR = 16, padT = 18, padB = 28, plotW = w - padL - padR, plotH = h - padT - padB;
    const totals = buckets.map(b => (b.cached_in || 0) + (b.uncached_in || 0) + (b.output || 0) + (b.reasoning || 0));
    const maxVal = Math.max(1, ...totals);
    ctx.strokeStyle = "rgba(255,255,255,0.055)", ctx.fillStyle = "#6d6d78";
    ctx.font = "10.5px ui-monospace, monospace", ctx.textAlign = "right";
    for (let i = 0; i <= 4; i++) {
      const y = padT + (plotH * i) / 4;
      ctx.beginPath(), ctx.moveTo(padL, y), ctx.lineTo(w - padR, y), ctx.stroke();
      ctx.fillText(fmtTokens(Math.round(maxVal * (1 - i / 4))), padL - 8, y + 4);
    }
    const step = plotW / buckets.length, barW = Math.max(8, Math.min(28, step * 0.68));
    const series = [["cached_in", "#22c55e"], ["uncached_in", "#f59e0b"], ["output", "#64748b"], ["reasoning", "#475061"]];
    buckets.forEach((b, idx) => {
      const x = padL + idx * step + (step - barW) / 2;
      let yCursor = padT + plotH;
      series.forEach(([key, color]) => {
        const val = b[key] || 0;
        if (!val) return;
        yCursor -= (val / maxVal) * plotH;
        ctx.fillStyle = color, ctx.fillRect(x, yCursor, barW, Math.max(1, (val / maxVal) * plotH));
      });
      if (idx % Math.max(1, Math.ceil(buckets.length / 8)) === 0) {
        const lbl = String(new Date(b.hour_ms || 0).getUTCHours()).padStart(2, "0") + ":00";
        ctx.fillStyle = "#6d6d78", ctx.textAlign = "center", ctx.fillText(lbl, x + barW / 2, h - 8);
      }
    });
  }
  // Frameless sparkline: the stroke is currentColor and the fill is a flat tint of the same
  // ink — no second gradient, no axes, no legend. Values are the caller's real series.
  function sparkline(svg, values, cls) {
    if (!svg) return;
    const w = 110, h = 28, v = (values || []).slice(-60), n = v.length;
    if (n < 2) return void (svg.innerHTML = '<line x1="0" y1="27" x2="110" y2="27" class="spark-idle"/>');
    const max = Math.max(...v), min = Math.min(...v), span = max - min || 1;
    const pts = v.map((val, i) => [(i / (n - 1)) * w, h - 3 - ((val - min) / span) * (h - 8)]);
    const line = pts.map((p, i) => `${i ? "L" : "M"}${p[0].toFixed(1)} ${p[1].toFixed(1)}`).join(" ");
    svg.setAttribute("viewBox", `0 0 ${w} ${h}`), svg.setAttribute("class", `spark ${cls || ""}`);
    svg.innerHTML = `<path class="spark-fill" d="${line} L${w} ${h} L0 ${h} Z"/><path class="spark-line" d="${line}"/>`;
  }
  const stateOf = (ev, badges) => {
    const sc = Number(ev.status_code || 200), b = badges || ev.badges || [];
    if (sc >= 500 || b.includes("breaker_trip")) return ["st-err", "rejected"];
    if (sc === 429 || b.includes("quota_429")) return ["st-warn", "cooldown"];
    if (b.includes("failover")) return ["st-warn", "failover"];
    return ["st-ok", "routed"];
  };
  const isCached = ev => !Number(ev.uncached_in || 0) && Number(ev.cached_in || 0) > 0;
  const STATE_BADGES = ["cache_hit", "failover", "quota_429", "breaker_trip"]; // restate the state word
  const barOf = (segs, width) => {
    const total = segs.reduce((a, [, v]) => a + v, 0);
    if (!total) return `<span class="bar" style="width:7%"></span>`;
    return `<span class="bar" style="width:${width}%">${segs.filter(([, v]) => v > 0)
      .map(([n, v]) => `<i class="seg-${n}" style="width:${(v / total) * 100}%"></i>`).join("")}</span>`;
  };
  // Bar length = input total vs the heaviest row on screen; the split inside it is the cache claim.
  function ledgerRow(ev, peak, idx) {
    const [cls, word] = stateOf(ev);
    const cached = Number(ev.cached_in || 0), paid = Number(ev.uncached_in || 0), saved = Number(ev.net_saved_usd || 0);
    const width = peak ? Math.max(7, Math.round(((cached + paid) / peak) * 100)) : 100;
    const badges = (ev.badges || []).filter(b => !STATE_BADGES.includes(b)).slice(0, 3).join(" · ");
    return `<div class="row mono" role="button" tabindex="0" data-idx="${idx}">`
      + `<span class="row-time">${new Date(ev.timestamp_ms || Date.now()).toISOString().slice(11, 19)}</span>`
      + `<span class="row-client">${esc(ev.client || "CLI")}</span>`
      + `<span class="row-target"><span class="t">${esc(pairLabel(ev.provider, ev.model))}</span>${badges ? `<span class="b">${esc(badges)}</span>` : ""}</span>`
      + `<span class="st ${cls}">${word}</span>${barOf([["cached", cached], ["uncached", paid]], width)}`
      + `<span class="row-tokens">${fmtTokens(cached)} / ${fmtTokens(paid)}</span>`
      + `<span class="row-ttft">${Number(ev.ttft_ms || 0)}ms</span>`
      + `<span class="row-saved ${signOf(saved)}">${fmtUsd(saved)}</span></div>`;
  }
  function providerRow(r) {
    const net = Number(r.net_saved_usd || 0), hit = Number(r.cache_hit_pct || 0);
    return `<tr><td>${esc(providerOf(r.provider, r.model))}</td><td class="mono"><span class="clip">${esc(r.model)}</span></td>`
      + `<td class="num">${Number(r.req || 0)}</td><td class="num">${Number(r.avg_ttft_ms || 0)}ms</td><td class="num ${hit > 0 ? "green" : "quiet"}">${hit.toFixed(1)}%</td>`
      + `<td class="num mono">${fmtTokens(r.cached_in)} / ${fmtTokens(r.uncached_in)}</td><td class="num mono">${fmtTokens(r.output)} (${fmtTokens(r.reasoning)})</td>`
      + `<td class="num ${signOf(net)}">${fmtUsd(r.net_saved_usd)}</td></tr>`;
  }
  /* Inspector drawer. The payload carries no total duration and no per-token price, so the
     latency bar is TTFT against the slowest visible request, and the tokens stay tokens. */
  function drawerHTML(ev, peakTtft) {
    const [cls, word] = stateOf(ev);
    const cached = Number(ev.cached_in || 0), paid = Number(ev.uncached_in || 0);
    const total = cached + paid, hit = total ? (cached / total) * 100 : 0;
    const ttft = Number(ev.ttft_ms || 0), slowest = Math.max(1, peakTtft || ttft);
    const kv = (k, v, c) => `<div class="kv"><dt>${k}</dt><dd class="${c || ""}">${v}</dd></div>`;
    return `<div class="drawer-head"><div>
        <div class="dw-ts mono">${new Date(ev.timestamp_ms || Date.now()).toISOString().replace("T", " ").slice(0, 19)}</div>
        <h2>${esc(pairLabel(ev.provider, ev.model))}</h2></div>
        <button class="btn btn-quiet" id="dw-close">Close</button></div>
      <dl class="kv-list">
        ${kv("Client", esc(ev.client || "CLI"))}
        ${kv("Status", `<span class="st ${cls}">${word}</span> <span class="quiet mono">${Number(ev.status_code || 200)}</span>`)}
        ${kv("Time to first token", `${ttft}ms`, "mono")}
        ${kv("Cache hit", `${hit.toFixed(1)}%`, hit > 0 ? "mono green" : "mono quiet")}
        ${kv("Cost delta", fmtUsd(ev.net_saved_usd), `mono ${signOf(Number(ev.net_saved_usd || 0))}`)}
        ${kv("Badges", esc((ev.badges || []).join(" · ") || "none"), "mono quiet")}
      </dl>
      <h3>Latency</h3>
      <div class="waterfall"><i style="width:${Math.min(100, (ttft / slowest) * 100)}%" class="${ttft ? "" : "empty"}"></i></div>
      <p class="dw-note">${ttft}ms to first token${ttft === slowest ? "" : ` — against ${slowest}ms, the slowest on screen`}.</p>
      <h3>Tokens</h3>
      <div class="split small"><i class="seg-cached" style="width:${hit}%"></i><i class="seg-uncached" style="width:${100 - hit}%"></i></div>
      <p class="dw-note mono">cached ${cached.toLocaleString()} · paid ${paid.toLocaleString()} · out ${Number(ev.output || 0).toLocaleString()} · reason ${Number(ev.reasoning || 0).toLocaleString()}</p>`;
  }

  /* ── graph painters ───────────────────────────────────────────────────
     Radial layout: clients on the left arc, models fanned across the right
     hemisphere as pills. Topology decides what moves; this decides what is drawn. */
  const NS = "http://www.w3.org/2000/svg";
  const G = { CX: 480, CY: 220, R: 62, CLIENT_R: 250, MODEL_R: 240, SPAN_C: 26, SPAN_M: 46 };
  // Provider identity lives on one 7px LED and nowhere else, so the outcome palette
  // (green routed / amber 429 / red 5xx) keeps the cables to itself.
  const TINTS = [[/^anthropic/, "#f59e0b"], [/^baidu|ernie/, "#22d3ee"], [/^mistral/, "#a78bfa"], [/^openai|gpt|o4/, "#22c55e"],
    [/^meta|llama|muse/, "#6366f1"], [/^google|gemini/, "#3b82f6"], [/^deepseek/, "#38bdf8"], [/^moonshot|kimi/, "#2dd4bf"]];
  const providerTint = key => (TINTS.find(([re]) => re.test(key)) || [, "#6d6d78"])[1];
  const mk = (tag, attrs, text) => {
    const n = document.createElementNS(NS, tag);
    for (const k in attrs || {}) n.setAttribute(k, String(attrs[k]));
    if (text != null) n.textContent = text;
    return n;
  };
  const nodeOf = (store, key) => (store.has(key) || store.set(key,
    { key, req: 0, seen: Date.now(), glow: 0, pt: { x: 0, y: 0 } }), store.get(key));
  // Orbital placement: one angle per node, spread symmetrically across its side's arc.
  const orb = (side, i, n) => {
    const span = side === "client" ? G.SPAN_C : G.SPAN_M, r = side === "client" ? G.CLIENT_R : G.MODEL_R;
    const deg = n <= 1 ? 0 : (i / (n - 1) - 0.5) * 2 * span, a = (deg * Math.PI) / 180;
    return { x: G.CX + (side === "client" ? -1 : 1) * r * Math.cos(a), y: G.CY + r * Math.sin(a) };
  };
  // The cable leaves the core exactly where it faces its node.
  const anchor = pt => {
    const a = Math.atan2(pt.y - G.CY, pt.x - G.CX);
    return { x: G.CX + G.R * Math.cos(a), y: G.CY + G.R * Math.sin(a) };
  };
  // Quadratic with a perpendicular bend: axons sweep, they do not run parallel.
  const wire = (a, b) => {
    const nx = -(b.y - a.y), ny = b.x - a.x, len = Math.hypot(nx, ny) || 1;
    const bend = Math.min(34, len * 0.17), cx = (a.x + b.x) / 2 + (nx / len) * bend, cy = (a.y + b.y) / 2 + (ny / len) * bend;
    return `M ${a.x.toFixed(1)} ${a.y.toFixed(1)} Q ${cx.toFixed(1)} ${cy.toFixed(1)} ${b.x.toFixed(1)} ${b.y.toFixed(1)}`;
  };
  const inCable = pt => wire({ x: pt.x + 12, y: pt.y }, anchor(pt));
  const outCable = pt => wire(anchor(pt), { x: pt.x + 2, y: pt.y });

  function graphKit(o) {
    const L = o.layers, clients = o.clients, models = o.models, edges = new Map();
    const place = (g, pt) => (g.setAttribute("transform", `translate(${pt.x} ${pt.y})`), g);
    function edge(key, d) {
      const p = edges.get(key) || edges.set(key, mk("path", { class: "edge" })).get(key);
      if (!p.parentNode) L.edges.appendChild(p); // a cleared layer re-adopts its cached path
      if (p.getAttribute("d") !== d) p.setAttribute("d", d);
      return (p._k = key), p; // every cable carries its key, so relays and hover can pair it
    }
    const reconcile = (store, keep) => store.forEach((n, k) => {
      if (!keep.includes(k) && n.g) n.g.remove(), store.delete(k);
    });
    // Clients: a bare dot with the name hung off it. Models: a capsule, because the
    // name and the count must share one line and never stack.
    const shell = (n, side) => {
      if (n.g) return n.g;
      const g = n.g = mk("g", { class: "n-node is-new" }), body = n.body = mk("g", { class: "nb" });
      if (side === "client") {
        body.append(mk("circle", { cx: 0, cy: 0, r: 10, class: "n-ring" }),
          mk("circle", { cx: 0, cy: 0, r: 3.2, class: "n-led" }),
          mk("text", { x: -16, y: 0, "text-anchor": "end", class: "n-label" }),
          mk("text", { x: -16, y: 14, "text-anchor": "end", class: "n-sub mono" }));
      } else {
        body.append(mk("rect", { x: 0, y: -11, height: 22, rx: 11, class: "n-pill" }),
          mk("circle", { cx: 12, cy: 0, r: 3.5, class: "n-led" }),
          mk("text", { x: 22, y: 4, class: "n-label" }),
          mk("text", { y: 4, class: "n-badge mono" }));
      }
      g.appendChild(body);
      (side === "client" ? L.agents : L.ups).appendChild(g);
      const key = `${side === "client" ? "c" : "m"}:${n.key}`;
      g.addEventListener("mouseenter", () => o.onFocus(key));
      g.addEventListener("mouseleave", () => o.onFocus(null));
      return g;
    };
    function drawClients(list) {
      list.forEach((c, i) => {
        const n = nodeOf(clients, mono(c.name));
        if (n.prev !== c.requests) n.seen = Date.now(), n.prev = c.requests;
        n.req = c.requests || 0, n.pt = orb("client", i, list.length);
        const g = place(shell(n, "client"), n.pt), idle = Date.now() - n.seen > 90000;
        n.body.children[1].style.fill = idle ? "#2a2a33" : "#22c55e";
        n.body.children[2].textContent = fit(n.key, 15), n.body.children[3].textContent = `${n.req} req`;
        g.classList.toggle("is-idle", idle);
      });
      reconcile(clients, list.map(c => mono(c.name)));
    }
    function drawModels(list) {
      const svgW = o.width ? o.width() : 1200, maxLabel = svgW < 760 ? 13 : svgW < 980 ? 19 : 26;
      const peak = Math.max(1, ...list.map(u => u.req_count || 0));
      list.forEach((u, i) => {
        const n = nodeOf(models, mono(u.label)), req = u.req_count || 0, err = u.error_count || 0;
        n.pt = { ...orb("model", i, list.length) }, n.req = req;
        const g = place(shell(n, "model"), n.pt);
        const [pill, led, label, badge] = n.body.children;
        // Width is clamped to the room left of the canvas edge: the equator node overflows first.
        const room = Math.max(9, Math.floor((948 - n.pt.x) / 6.4) - 12), text = fit(n.key, Math.min(maxLabel, room)), w = 34 + text.length * 6.4 + 44;
        led.style.fill = providerTint(n.key), pill.setAttribute("width", w.toFixed(0));
        label.textContent = text, badge.setAttribute("x", (w - 11).toFixed(0)), badge.textContent = err ? `${req} · ${err}✕` : `${req} req`;
        g.classList.toggle("is-err", err > 0);
      });
      reconcile(models, list.map(u => mono(u.label)));
      return peak;
    }
    return { edge, drawClients, drawModels };
  }

  return {
    fmtTokens, fmtUsd, signOf, providerOf, pairLabel, cleanModel, mk, nodeOf, geo: G,
    renderStackedChart, sparkline, ledgerRow, providerRow, drawerHTML, isCached, stateOf, graphKit,
    orb, anchor, wire, inCable, outCable, providerTint,
  };
})();
