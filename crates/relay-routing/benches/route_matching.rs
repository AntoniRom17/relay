//! Criterion baseline for the compiled route-matching hot path.

use std::{collections::BTreeMap, hint::black_box};

use criterion::{Criterion, criterion_group, criterion_main};
use http::Request;
use relay_config::{RetryConfig, RouteConfig};
use relay_routing::RouteTable;

fn route(index: usize) -> RouteConfig {
    RouteConfig {
        name: format!("tenant-{index}"),
        host: Some("api.example.test".to_owned()),
        path_exact: None,
        path_prefix: Some(format!("/tenant/{index}")),
        methods: vec!["GET".to_owned()],
        headers: BTreeMap::new(),
        upstream: "application".to_owned(),
        retry: RetryConfig::default(),
        rate_limit: None,
        authentication: None,
        cache: None,
        max_body_bytes: 1024 * 1024,
        concurrency_limit: 512,
    }
}

fn match_routes(criterion: &mut Criterion) {
    let table = RouteTable::compile((0..256).map(route).collect()).expect("valid routes");
    let request = Request::get("/tenant/255/items?limit=10")
        .header("host", "api.example.test")
        .body(())
        .expect("request");

    criterion.bench_function("route_match/256/late_prefix", |bencher| {
        bencher.iter(|| black_box(table.match_request(black_box(&request))))
    });
}

criterion_group!(benches, match_routes);
criterion_main!(benches);
