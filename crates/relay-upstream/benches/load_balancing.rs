//! Criterion baselines for lock-free endpoint selection policies.

use std::{hint::black_box, sync::Arc};

use criterion::{Criterion, criterion_group, criterion_main};
use relay_upstream::{Endpoint, LeastConnections, LoadBalancer, RoundRobin, WeightedRoundRobin};

fn endpoints() -> Vec<Arc<Endpoint>> {
    (0_u16..16)
        .map(|index| {
            Arc::new(Endpoint::new(
                format!("http://127.0.0.1:{}", 9000 + index)
                    .parse()
                    .expect("valid endpoint"),
                u32::from(index % 4) + 1,
            ))
        })
        .collect()
}

fn select_endpoints(criterion: &mut Criterion) {
    let endpoints = endpoints();
    let round_robin = RoundRobin::new();
    let weighted = WeightedRoundRobin::new(&endpoints);
    let least = LeastConnections;
    let mut group = criterion.benchmark_group("load_balancer/16_healthy");
    group.bench_function("round_robin", |bencher| {
        bencher.iter(|| round_robin.select(black_box(&endpoints)));
    });
    group.bench_function("weighted_round_robin", |bencher| {
        bencher.iter(|| weighted.select(black_box(&endpoints)));
    });
    group.bench_function("least_connections", |bencher| {
        bencher.iter(|| least.select(black_box(&endpoints)));
    });
    group.finish();
}

criterion_group!(benches, select_endpoints);
criterion_main!(benches);
