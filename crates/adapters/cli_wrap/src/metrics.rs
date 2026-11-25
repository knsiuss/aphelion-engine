use aphelion_core::flight_recorder::normalize_provider_label;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

#[derive(Debug, Default, Clone)]
struct ProviderModelMetrics {
    req_2xx: u64,
    req_4xx: u64,
    req_5xx: u64,
    cached_tokens: u64,
    uncached_tokens: u64,
    output_tokens: u64,
    reasoning_tokens: u64,
    ttft_ms_sum: u64,
    ttft_ms_count: u64,
    compactions_total: u64,
}

/// Zero-dependency labeled Prometheus metrics registry for `GET /metrics`.
#[derive(Default)]
pub struct MetricsRegistry {
    rate_limited_total: AtomicU64,
    entries: Mutex<BTreeMap<(String, String), ProviderModelMetrics>>,
}

impl MetricsRegistry {
    pub fn record_rate_limited(&self) {
        self.rate_limited_total.fetch_add(1, Ordering::Relaxed);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_request(
        &self,
        provider: &str,
        model: &str,
        status_code: u16,
        ttft_ms: u64,
        cached_tokens: u64,
        uncached_tokens: u64,
        output_tokens: u64,
        reasoning_tokens: u64,
        compaction_triggered: bool,
    ) {
        let prov = normalize_provider_label(provider);
        let mdl = if model.trim().is_empty() {
            "unknown".to_owned()
        } else {
            model.trim().to_owned()
        };
        let Ok(mut map) = self.entries.lock() else {
            return;
        };
        let key = if map.len() >= 256 && !map.contains_key(&(prov.clone(), mdl.clone())) {
            ("other".to_owned(), "other".to_owned())
        } else {
            (prov, mdl)
        };
        let entry = map.entry(key).or_default();
        match status_code {
            200..=299 => entry.req_2xx += 1,
            400..=499 => entry.req_4xx += 1,
            _ => entry.req_5xx += 1,
        }
        entry.cached_tokens = entry.cached_tokens.saturating_add(cached_tokens);
        entry.uncached_tokens = entry.uncached_tokens.saturating_add(uncached_tokens);
        entry.output_tokens = entry.output_tokens.saturating_add(output_tokens);
        entry.reasoning_tokens = entry.reasoning_tokens.saturating_add(reasoning_tokens);
        entry.ttft_ms_sum = entry.ttft_ms_sum.saturating_add(ttft_ms);
        entry.ttft_ms_count += 1;
        if compaction_triggered {
            entry.compactions_total += 1;
        }
    }

    pub fn render_prometheus(&self) -> String {
        let mut out = String::with_capacity(1024);
        let r429 = self.rate_limited_total.load(Ordering::Relaxed);
        let snapshot: Vec<((String, String), ProviderModelMetrics)> = self
            .entries
            .lock()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();

        let _ = writeln!(out, "# TYPE aphelion_requests_total counter");
        for ((prov, mdl), m) in &snapshot {
            let p = escape_prom_label(prov);
            let ml = escape_prom_label(mdl);
            if m.req_2xx > 0 {
                let _ = writeln!(
                    out,
                    r#"aphelion_requests_total{{provider="{p}",model="{ml}",status_class="2xx"}} {}"#,
                    m.req_2xx
                );
            }
            if m.req_4xx > 0 {
                let _ = writeln!(
                    out,
                    r#"aphelion_requests_total{{provider="{p}",model="{ml}",status_class="4xx"}} {}"#,
                    m.req_4xx
                );
            }
            if m.req_5xx > 0 {
                let _ = writeln!(
                    out,
                    r#"aphelion_requests_total{{provider="{p}",model="{ml}",status_class="5xx"}} {}"#,
                    m.req_5xx
                );
            }
        }

        let _ = writeln!(out, "# TYPE aphelion_tokens_total counter");
        for ((prov, mdl), m) in &snapshot {
            let p = escape_prom_label(prov);
            let ml = escape_prom_label(mdl);
            let _ = writeln!(
                out,
                r#"aphelion_tokens_total{{provider="{p}",model="{ml}",type="cached"}} {}"#,
                m.cached_tokens
            );
            let _ = writeln!(
                out,
                r#"aphelion_tokens_total{{provider="{p}",model="{ml}",type="uncached"}} {}"#,
                m.uncached_tokens
            );
            let _ = writeln!(
                out,
                r#"aphelion_tokens_total{{provider="{p}",model="{ml}",type="output"}} {}"#,
                m.output_tokens
            );
            let _ = writeln!(
                out,
                r#"aphelion_tokens_total{{provider="{p}",model="{ml}",type="reasoning"}} {}"#,
                m.reasoning_tokens
            );
        }

        let _ = writeln!(out, "# TYPE aphelion_ttft_ms_sum counter");
        let _ = writeln!(out, "# TYPE aphelion_ttft_ms_count counter");
        for ((prov, mdl), m) in &snapshot {
            let p = escape_prom_label(prov);
            let ml = escape_prom_label(mdl);
            let _ = writeln!(
                out,
                r#"aphelion_ttft_ms_sum{{provider="{p}",model="{ml}"}} {}"#,
                m.ttft_ms_sum
            );
            let _ = writeln!(
                out,
                r#"aphelion_ttft_ms_count{{provider="{p}",model="{ml}"}} {}"#,
                m.ttft_ms_count
            );
        }

        let _ = writeln!(out, "# TYPE aphelion_compactions_total counter");
        for ((prov, mdl), m) in &snapshot {
            let p = escape_prom_label(prov);
            let ml = escape_prom_label(mdl);
            let _ = writeln!(
                out,
                r#"aphelion_compactions_total{{provider="{p}",model="{ml}"}} {}"#,
                m.compactions_total
            );
        }

        let _ = writeln!(out, "# TYPE aphelion_rate_limited_total counter");
        let _ = writeln!(out, "aphelion_rate_limited_total {r429}");
        out
    }
}

/// Escapes Prometheus text exposition label values (`\`, `"`, `\n`, `\r`).
pub fn escape_prom_label(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
    out
}
