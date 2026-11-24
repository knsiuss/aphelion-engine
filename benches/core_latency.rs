use aphelion_core::types::{ExecutionProfile, SessionState};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use serde_json::json;

fn build_payload(target_bytes: usize) -> Vec<u8> {
    let unit = "The parser validates token spans and preserves code blocks without mutating frozen prefixes. ";
    let repeats = (target_bytes / unit.len()).max(1);
    let body = unit.repeat(repeats);
    serde_json::to_vec(&json!({
        "model": "gpt-4o",
        "system": "You are a principal Rust engineer. cwd: /workspace/aphelion",
        "messages": [
            { "role": "user", "content": body }
        ]
    }))
    .unwrap_or_default()
}

fn bench_core_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("aphelion_core_ephemeral");
    for &(label, size) in &[("10KB", 10_240usize), ("50KB", 51_200usize), ("200KB", 204_800usize)] {
        let payload = build_payload(size);
        group.bench_with_input(BenchmarkId::new("prepare_request", label), &payload, |b, data| {
            b.iter(|| {
                let mut state = SessionState::default();
                let res = aphelion_core::prepare_request(
                    black_box(data),
                    ExecutionProfile::Ephemeral,
                    &mut state,
                );
                black_box(res.ok())
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_core_latency);
criterion_main!(benches);
