window.AphelionCharts = (() => {
  function fmtTokens(n) {
    if (n >= 1e6) return (n / 1e6).toFixed(1) + "M";
    if (n >= 1e3) return (n / 1e3).toFixed(1) + "k";
    return String(n || 0);
  }

  function fmtUsd(v) {
    const num = Number(v || 0);
    return (num < 0 ? "-$" : "$") + Math.abs(num).toFixed(4);
  }

  function renderStackedChart(canvas, buckets) {
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    const w = canvas.width;
    const h = canvas.height;
    ctx.clearRect(0, 0, w, h);

    if (!buckets || !buckets.length) {
      ctx.fillStyle = "#a1a1aa";
      ctx.font = "13px monospace";
      ctx.textAlign = "center";
      ctx.fillText("No token timeseries recorded yet", w / 2, h / 2);
      return;
    }

    const padL = 56, padR = 16, padT = 18, padB = 28;
    const plotW = w - padL - padR;
    const plotH = h - padT - padB;
    const totals = buckets.map(b => (b.cached_in || 0) + (b.uncached_in || 0) + (b.output || 0) + (b.reasoning || 0));
    const maxVal = Math.max(1, ...totals);

    ctx.strokeStyle = "#27272a";
    ctx.fillStyle = "#a1a1aa";
    ctx.font = "11px monospace";
    ctx.textAlign = "right";
    for (let i = 0; i <= 4; i++) {
      const y = padT + (plotH * i) / 4;
      ctx.beginPath();
      ctx.moveTo(padL, y);
      ctx.lineTo(w - padR, y);
      ctx.stroke();
      const labelVal = Math.round(maxVal * (1 - i / 4));
      ctx.fillText(fmtTokens(labelVal), padL - 8, y + 4);
    }

    const step = plotW / buckets.length;
    const barW = Math.max(8, Math.min(28, step * 0.68));
    const series = [
      ["cached_in", "#22c55e"],
      ["uncached_in", "#f59e0b"],
      ["output", "#38bdf8"],
      ["reasoning", "#a855f7"],
    ];

    buckets.forEach((b, idx) => {
      const x = padL + idx * step + (step - barW) / 2;
      let yCursor = padT + plotH;
      series.forEach(([key, color]) => {
        const val = b[key] || 0;
        if (!val) return;
        const segH = (val / maxVal) * plotH;
        yCursor -= segH;
        ctx.fillStyle = color;
        ctx.fillRect(x, yCursor, barW, Math.max(1, segH));
      });
      if (idx % Math.max(1, Math.ceil(buckets.length / 8)) === 0) {
        const d = new Date(b.hour_ms || 0);
        const lbl = String(d.getUTCHours()).padStart(2, "0") + ":00";
        ctx.fillStyle = "#a1a1aa";
        ctx.textAlign = "center";
        ctx.fillText(lbl, x + barW / 2, h - 8);
      }
    });
  }

  return { fmtTokens, fmtUsd, renderStackedChart };
})();
