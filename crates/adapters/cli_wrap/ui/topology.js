window.AphelionTopology = (() => {
  const NS = "http://www.w3.org/2000/svg";
  const edgeIndex = new Map();

  function el(tag, attrs, text) {
    const node = document.createElementNS(NS, tag);
    for (const [k, v] of Object.entries(attrs || {})) node.setAttribute(k, String(v));
    if (text !== undefined) node.textContent = text;
    return node;
  }

  function healthColor(h) {
    if (h === "cooldown_429") return "#f59e0b";
    if (h === "degraded") return "#ef4444";
    return "#22c55e";
  }

  function render(svg, topo) {
    if (!svg || !topo) return;
    svg.innerHTML = "";
    edgeIndex.clear();

    const clients = (topo.clients && topo.clients.length) ? topo.clients : [{ name: "CLI", requests: 0 }];
    const upstreams = (topo.upstreams && topo.upstreams.length)
      ? topo.upstreams
      : [{ label: "auto-detect", health: "healthy", avg_ttft_ms: 0, req_count: 0 }];
    const eng = topo.engine || {};

    const cx = 370, cy = 105, cw = 220, ch = 130;
    const cStep = Math.min(68, 280 / Math.max(1, clients.length));
    const uStep = Math.min(68, 280 / Math.max(1, upstreams.length));
    const cStart = 170 - ((clients.length - 1) * cStep) / 2;
    const uStart = 170 - ((upstreams.length - 1) * uStep) / 2;

    const edgesG = el("g");
    const nodesG = el("g");
    svg.appendChild(edgesG);
    svg.appendChild(nodesG);

    clients.slice(0, 5).forEach((c, i) => {
      const y = cStart + i * cStep;
      const d = `M 210 ${y} C 290 ${y}, 290 170, ${cx} 170`;
      const p = el("path", { d, class: "edge-path" });
      edgesG.appendChild(p);
      edgeIndex.set(`c:${c.name}`, p);

      nodesG.appendChild(el("rect", { x: 24, y: y - 22, width: 186, height: 44, class: "node-box" }));
      nodesG.appendChild(el("text", { x: 38, y: y - 3, fill: "#f4f4f5", "font-size": 12, "font-weight": 600 }, c.name));
      nodesG.appendChild(el("text", { x: 38, y: y + 13, fill: "#a1a1aa", "font-size": 11 }, `${c.requests} req`));
    });

    upstreams.slice(0, 5).forEach((u, i) => {
      const y = uStart + i * uStep;
      const d = `M ${cx + cw} 170 C 670 170, 670 ${y}, 740 ${y}`;
      const p = el("path", { d, class: "edge-path" });
      edgesG.appendChild(p);
      edgeIndex.set(`u:${u.label}`, p);

      nodesG.appendChild(el("rect", { x: 740, y: y - 22, width: 196, height: 44, class: "node-box" }));
      nodesG.appendChild(el("circle", { cx: 756, cy: y - 5, r: 5, fill: healthColor(u.health) }));
      nodesG.appendChild(el("text", { x: 768, y: y - 1, fill: "#f4f4f5", "font-size": 12, "font-weight": 600 }, u.label.slice(0, 20)));
      nodesG.appendChild(el("text", { x: 752, y: y + 14, fill: "#a1a1aa", "font-size": 11 }, `${u.req_count} req · ${u.avg_ttft_ms}ms`));
    });

    nodesG.appendChild(el("rect", { x: cx, y: cy, width: cw, height: ch, class: "node-engine" }));
    nodesG.appendChild(el("text", { x: cx + 16, y: cy + 26, fill: "#38bdf8", "font-size": 13, "font-weight": 700 }, "APHELION ENGINE"));
    const pills = [
      `PrefixLedger: ${Number(eng.global_hit_pct || 0).toFixed(1)}% hit`,
      `OutputOpt: ${eng.output_mode || "concise"}`,
      `Compaction: ${eng.compaction_mode || "auto"}`,
      `Reasoning: ${eng.reasoning_effort || "default"}`,
    ];
    pills.forEach((txt, idx) => {
      nodesG.appendChild(el("text", { x: cx + 16, y: cy + 52 + idx * 19, fill: "#e4e4e7", "font-size": 11 }, txt));
    });
  }

  function pulse(clientName, providerLabel) {
    [edgeIndex.get(`c:${clientName}`), edgeIndex.get(`u:${providerLabel}`)].forEach(p => {
      if (!p) return;
      p.classList.add("pulse");
      setTimeout(() => p.classList.remove("pulse"), 700);
    });
  }

  return { render, pulse };
})();
