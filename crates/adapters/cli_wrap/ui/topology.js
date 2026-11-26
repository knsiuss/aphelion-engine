window.AphelionTopology = (() => {
  const T = () => window.AphelionCharts || {};
  const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;
  const tint = c => (c >= 500 ? "#ef4444" : c >= 400 ? "#f59e0b" : "#22c55e");
  const mono = v => String(v == null ? "" : v);
  const clients = new Map(), models = new Map(), flying = [], relays = [], streams = new Map();
  const nodeKeyFor = m => [...models.keys()].find(k => k === m || k.endsWith("/" + m)) || null;
  let root = null, L = null, K = null, raf = 0, prev = 0, coreKey = "", lastTopo = null, lastRows = null, lastRecent = [], paused = false;

  function buildBase() {
    root.querySelectorAll("g.topo-layer").forEach(n => n.remove());
    clients.clear(), models.clear(), flying.length = 0, L = {};
    ["edges", "relays", "core", "packets", "ups", "agents"].forEach(k => L[k] = root.appendChild(T().mk("g", { class: "topo-layer" })));
    K = T().graphKit({ layers: L, clients, models, onFocus: setFocus, width: () => root.getBoundingClientRect().width });
  }

  // Machined hub, not a glowing orb: thin rings, a measured arc, type as the only mass.
  function drawCore(eng) {
    const g = L.core, G = T().geo, n = Number(eng.active_sessions || 0), circ = 2 * Math.PI * (G.R - 5);
    const hit = Math.max(0, Math.min(100, Number(eng.global_hit_pct || 0)));
    g.innerHTML = "";
    g.append(
      T().mk("circle", { cx: G.CX, cy: G.CY, r: G.R, class: "core-orbit" }),
      T().mk("circle", { cx: G.CX, cy: G.CY, r: G.R - 5, class: "core-track" }),
      T().mk("circle", {
        cx: G.CX, cy: G.CY, r: G.R - 5, class: "core-arc", transform: `rotate(-90 ${G.CX} ${G.CY})`,
        "stroke-dasharray": `${((hit / 100) * circ).toFixed(1)} ${circ.toFixed(1)}`,
      }),
      T().mk("circle", { cx: G.CX, cy: G.CY, r: G.R - 8, class: "core-disc" }),
      T().mk("circle", { cx: G.CX, cy: G.CY, r: G.R - 8, class: "core-wave" }),
    );
    const t = (dy, cls, s) => g.appendChild(T().mk("text", { x: G.CX, y: G.CY + dy, "text-anchor": "middle", class: cls }, s));
    t(-12, "core-name", "APHELION"), t(12, "core-stat mono", String(n)), t(26, "core-cap", "sessions");
  }

  function coreFire(color) {
    const wave = L && L.core.querySelector(".core-wave");
    if (!wave || reduced) return;
    wave.setAttribute("stroke", color), wave.classList.remove("is-fire");
    void wave.getBoundingClientRect(), wave.classList.add("is-fire");
  }

  function render(svg, topo, rows, recent) {
    if (!svg || !topo) return;
    if (root !== svg) { root = svg; buildBase(); }
    lastTopo = topo, lastRows = rows || lastRows, lastRecent = recent || lastRecent;
    L.edges.querySelectorAll("path").forEach(p => p.remove());
    const cList = (topo.clients || []).slice(0, 7);
    const clean = (lastRows || []).filter(r => !T().cleanModel || T().cleanModel(r.provider, r.model));
    // Per-pair rows carry the real provider; upstream labels collapse every combo route to one host.
    const uList = clean.length
      ? clean.slice(0, 9).map(r => {
        const lab = T().pairLabel(r.provider, r.model), n = models.get(lab), cool = n && n.cool && Date.now() - n.cool < 60000;
        return {
          label: lab, req_count: r.req, avg_ttft_ms: r.avg_ttft_ms, error_count: n && n.errs || 0,
          health: cool ? "cooldown_429" : n && n.errs >= 2 ? "degraded" : "healthy",
        };
      })
      : (topo.upstreams || []).slice(0, 7);
    K.drawClients(cList);
    const peak = K.drawModels(uList);
    cList.forEach(c => {
      const n = clients.get(mono(c.name));
      if (!n) return;
      K.edge(`c:${n.key}`, T().inCable(n.pt)).classList.toggle("is-live", c.requests > 0);
    });
    uList.forEach(u => {
      const n = models.get(mono(u.label));
      if (!n) return;
      const p = K.edge(`m:${n.key}`, T().outCable(n.pt));
      p.classList.toggle("is-live", u.req_count > 0);
      p.style.strokeWidth = (1.2 + 2.6 * ((u.req_count || 0) / peak)).toFixed(2); // class pins 1.2
    });
    seedRelays();
    const eng = topo.engine || {}, key = `${eng.active_sessions}|${eng.global_hit_pct}|${uList.length}|${cList.length}`;
    if (key !== coreKey) drawCore(eng), coreKey = key;
    svg.setAttribute("aria-label", `Live routing graph: ${cList.length} clients, ${uList.length} models, ${flying.length} signals in flight`);
    setTimeout(() => [...clients.values(), ...models.values()].forEach(n => n.g && n.g.classList.remove("is-new")), 420);
    const note = document.getElementById("topo-empty");
    if (note) note.hidden = (uList.length + cList.length) > 0;
    startLoop();
  }

  function seedRelays() {
    L.relays.innerHTML = "", relays.length = 0;
    L.edges.querySelectorAll("path").forEach(p => {
      const len = p.getTotalLength() || 1;
      [0.34, 0.7].forEach(t => {
        const q = p.getPointAtLength(len * t);
        const el = T().mk("circle", { cx: q.x.toFixed(1), cy: q.y.toFixed(1), r: 1.7, class: "n-relay" });
        el._k = p._k, el._t = t, L.relays.appendChild(el), relays.push(el);
      });
    });
  }

  // A stream may name a pair the API has not aggregated yet: seed the tier now, replace on next poll.
  function grow(which, key) {
    if (!lastTopo || (lastTopo[which] || []).some(x => mono(x.name || x.label) === key)) return key;
    const seed = which === "clients" ? { name: key, requests: 1 }
      : { label: key, health: "healthy", req_count: 1, avg_ttft_ms: 0, error_count: 0 };
    render(root, Object.assign({}, lastTopo, { [which]: [...(lastTopo[which] || []), seed] }), lastRows, lastRecent);
    return key;
  }

  function pulseAlong(path, dur, color, done) {
    if (!path) return done && done();
    const key = path._k;
    const head = T().mk("circle", { r: 2.6, fill: color, class: "pkt" });
    const trail = T().mk("path", { stroke: color, class: "pkt-trail" });
    L.packets.append(trail, head);
    flying.push({ path, key, head, trail, t: 0, dur, done, hist: [], len: 0 });
    startLoop();
  }
  function flashEdge(p, cls, color, ms) {
    if (!p) return;
    p.classList.add(cls), p.style.stroke = color;
    setTimeout(() => { p.classList.remove(cls); p.style.stroke = ""; }, ms);
  }

  /* One entry point, called straight from the SSE handler in app.js. Three shapes:
     cached (hub only), routed (hub → model), failed (red cable, then amber return to hub). */
  function pulse(ev) {
    if (!L || paused) return;
    const status = Number(ev.status_code || 200);
    const clean = T().cleanModel ? T().cleanModel("", ev.model) : ev.model; // provider may echo the id
    if (!clean) return;
    const cKey = grow("clients", mono(ev.client || "CLI"));
    const mKey = grow("upstreams", nodeKeyFor(clean) || T().pairLabel(ev.provider, clean));
    const cn = T().nodeOf(clients, cKey), mn = T().nodeOf(models, mKey), color = tint(status);
    const cached = T().isCached ? T().isCached(ev) : false;
    if (status >= 500) mn.errs = (mn.errs || 0) + 1; else if (status === 429) mn.cool = Date.now();
    cn.glow = 1, cn.seen = Date.now();
    if (cn.g) cn.g.classList.add("is-hot"), cn.g.classList.remove("is-idle");
    if (reduced) { if (mn.g) mn.g.classList.add("is-hot", "is-new"); return; }
    const out = K.edge(`m:${mKey}`, T().outCable(mn.pt.y ? mn.pt : { x: T().geo.CX + 120, y: T().geo.CY }));
    pulseAlong(K.edge(`c:${cKey}`, T().inCable(cn.pt.y ? cn.pt : { x: T().geo.CX - 120, y: T().geo.CY })), cached ? 0.28 : 0.42, cached ? "#22c55e" : color, () => {
      coreFire(cached ? "#22c55e" : color);
      if (cached) { mn.glow = 1; return; } // nothing new to send upstream on a full cache hit
      if (status >= 500 || status === 429) {
        flashEdge(out, "is-fault", "#ef4444", 600);
        setTimeout(() => pulseAlong(out, 0.5, "#f59e0b", () => { mn.glow = 1; }), 260);
        return;
      }
      pulseAlong(out, 0.5, color, () => {
        if (mn.g) mn.g.classList.add("is-hot");
        mn.glow = 1, flashEdge(out, "is-stream", color, 1400);
      });
    });
  }

  // Hover dims every cable off the inspected chain. The aggregate payload has no client-to-model
  // pairing, but the per-request rows do, so the chain drawn is the path traffic actually took.
  function setFocus(key) {
    if (!root) return;
    const side = (key || "")[0], name = (key || "").slice(2), links = new Set();
    (lastRecent || []).forEach(r => {
      const m = nodeKeyFor(T().cleanModel("", r.model)), c = mono(r.client || "CLI");
      if (!m) return;
      if (side === "c" && c === name) links.add(`m:${m}`);
      if (side === "m" && m === name) links.add(`c:${c}`);
    });
    root.classList.toggle("has-focus", !!key);
    L.edges.querySelectorAll("path").forEach(p => p.classList.toggle("is-chain",
      !!key && (p._k === key || links.has(p._k))));
  }

  function step(now) {
    const dt = Math.min(0.05, (now - prev) / 1000);
    prev = now;
    let busy = false;
    flying.slice().forEach(f => {
      f.t = Math.min(1, f.t + dt / f.dur);
      if (!f.len) f.len = f.path.getTotalLength() || 1;
      const pos = f.path.getPointAtLength(f.len * f.t);
      f.head.setAttribute("cx", pos.x), f.head.setAttribute("cy", pos.y);
      f.hist.push({ x: pos.x, y: pos.y });
      if (f.hist.length > 7) f.hist.shift();
      f.trail.setAttribute("d", f.hist.map((p, j) => `${j ? "L" : "M"} ${p.x.toFixed(1)} ${p.y.toFixed(1)}`).join(" "));
      f.trail.setAttribute("opacity", String(0.16 + (0.5 * f.hist.length) / 7));
      if (f.t < 1) { busy = true; return; }
      f.head.remove(); f.trail.remove(); // no orphaned pulse nodes
      flying.splice(flying.indexOf(f), 1);
      if (f.done) f.done();
    });
    let lit = false;
    const fade = store => store.forEach(n => {
      if (n.glow <= 0) return;
      n.glow = Math.max(0, n.glow - dt * 1.6), lit = true;
      if (n.g) n.g.classList.toggle("is-hot", n.glow > 0.02);
    });
    fade(clients), fade(models);
    relays.forEach(d => d.classList.toggle("is-lit",
      flying.some(f => f.key === d._k && Math.abs(f.t - d._t) < 0.09)));
    root.classList.toggle("is-flow", busy);
    if (!busy && !lit) { raf = 0; return; }
    raf = requestAnimationFrame(step);
  }
  function startLoop() {
    if (raf || paused || !flying.length) return;
    prev = performance.now(), raf = requestAnimationFrame(step);
  }

  // Anchors are user units, so a resize only changes label sizes — never the geometry.
  if (typeof ResizeObserver !== "undefined") {
    let t = 0;
    new ResizeObserver(() => {
      clearTimeout(t);
      t = setTimeout(() => { if (root && lastTopo && !paused) render(root, lastTopo, lastRows, lastRecent); }, 180);
    }).observe(document.documentElement);
  }

  return {
    render, pulse, concurrency: () => flying.length,
    pause: on => {
      paused = !!on;
      if (paused) { if (raf) cancelAnimationFrame(raf); raf = 0; if (root) root.classList.remove("is-flow", "has-focus"); }
      else startLoop();
    },
  };
})();
