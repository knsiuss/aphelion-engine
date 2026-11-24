use std::env;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct VllmMetricsSnapshot {
    pub gpu_prefix_cache_hit_rate: Option<f64>,
    pub ttft_sum_seconds: Option<f64>,
    pub ttft_count: Option<f64>,
}

impl VllmMetricsSnapshot {
    pub fn mean_ttft_ms(&self) -> Option<f64> {
        let sum = self.ttft_sum_seconds?;
        let count = self.ttft_count?;
        if count <= 0.0 {
            return None;
        }
        Some((sum / count) * 1000.0)
    }
}

/// Parses Prometheus `/metrics` exposition text for vLLM prefix cache hit rate and TTFT.
pub fn parse_vllm_metrics(body: &str) -> VllmMetricsSnapshot {
    let mut snap = VllmMetricsSnapshot::default();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with("vllm:gpu_prefix_cache_hit_rate") {
            snap.gpu_prefix_cache_hit_rate = parse_prom_value(trimmed);
        } else if trimmed.starts_with("vllm:time_to_first_token_seconds_sum") {
            snap.ttft_sum_seconds = parse_prom_value(trimmed);
        } else if trimmed.starts_with("vllm:time_to_first_token_seconds_count") {
            snap.ttft_count = parse_prom_value(trimmed);
        }
    }
    snap
}

fn parse_prom_value(line: &str) -> Option<f64> {
    line.split_whitespace().last()?.parse::<f64>().ok()
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let url = if args.len() >= 2 && args[0] == "--url" {
        args[1].clone()
    } else {
        "http://127.0.0.1:8000/metrics".to_owned()
    };

    match reqwest::get(&url).await {
        Ok(resp) => match resp.text().await {
            Ok(text) => {
                let snap = parse_vllm_metrics(&text);
                println!("vLLM Probe ({url}):");
                println!(
                    "  gpu_prefix_cache_hit_rate = {:?}",
                    snap.gpu_prefix_cache_hit_rate
                );
                println!("  mean_ttft_ms              = {:?}", snap.mean_ttft_ms());
            }
            Err(err) => eprintln!("failed to read metrics body: {err}"),
        },
        Err(err) => eprintln!("failed to connect to {url}: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vllm_prometheus_metrics() {
        let sample = r#"
# HELP vllm:gpu_prefix_cache_hit_rate GPU prefix cache hit rate
# TYPE vllm:gpu_prefix_cache_hit_rate gauge
vllm:gpu_prefix_cache_hit_rate{model_name="qwen2.5-coder"} 0.875
vllm:time_to_first_token_seconds_sum{model_name="qwen2.5-coder"} 1.25
vllm:time_to_first_token_seconds_count{model_name="qwen2.5-coder"} 10
"#;
        let snap = parse_vllm_metrics(sample);
        assert_eq!(snap.gpu_prefix_cache_hit_rate, Some(0.875));
        assert_eq!(snap.mean_ttft_ms(), Some(125.0));
    }
}
