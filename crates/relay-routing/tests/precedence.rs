//! Behavioral and property tests for routing precedence.

use std::collections::BTreeMap;

use http::Request;
use proptest::prelude::*;
use relay_config::{RetryConfig, RouteConfig};
use relay_routing::RouteTable;

fn route(name: &str, prefix: Option<&str>) -> RouteConfig {
    RouteConfig {
        name: name.to_owned(),
        host: None,
        path_exact: None,
        path_prefix: prefix.map(str::to_owned),
        methods: Vec::new(),
        headers: BTreeMap::new(),
        upstream: "test".to_owned(),
        retry: RetryConfig::default(),
        rate_limit: None,
        authentication: None,
        cache: None,
        max_body_bytes: 1_024,
        concurrency_limit: 16,
    }
}

fn matched_name(routes: Vec<RouteConfig>, uri: &str, host: &str) -> String {
    let table = RouteTable::compile(routes).expect("test routes should compile");
    let request = Request::builder()
        .uri(uri)
        .header("host", host)
        .body(())
        .expect("test request should build");
    table
        .match_request(&request)
        .expect("a route should match")
        .route
        .name
        .clone()
}

#[test]
fn exact_path_wins_over_prefix_and_catch_all() {
    let catch_all = route("catch-all", None);
    let prefix = route("prefix", Some("/api"));
    let mut exact = route("exact", None);
    exact.path_exact = Some("/api/users".to_owned());

    assert_eq!(
        matched_name(vec![catch_all, prefix, exact], "/api/users", "example.test"),
        "exact"
    );
}

#[test]
fn host_specific_route_wins_and_host_port_is_ignored() {
    let generic = route("generic", Some("/"));
    let mut hosted = route("hosted", Some("/"));
    hosted.host = Some("API.Example.Test".to_owned());

    assert_eq!(
        matched_name(vec![generic, hosted], "/resource", "api.example.test:8443"),
        "hosted"
    );
}

#[test]
fn method_and_header_constraints_outrank_broader_routes() {
    let broad = route("broad", Some("/api"));
    let mut constrained = route("constrained", Some("/api"));
    constrained.methods = vec!["POST".to_owned()];
    constrained
        .headers
        .insert("x-tenant".to_owned(), "blue".to_owned());
    let table = RouteTable::compile(vec![broad, constrained]).expect("routes should compile");
    let request = Request::post("/api/jobs")
        .header("host", "example.test")
        .header("x-tenant", "blue")
        .body(())
        .expect("request should build");

    assert_eq!(
        table.match_request(&request).expect("route").route.name,
        "constrained"
    );
}

#[test]
fn declaration_order_breaks_equal_specificity_ties() {
    let first = route("first", Some("/api"));
    let second = route("second", Some("/api"));

    assert_eq!(
        matched_name(vec![first, second], "/api/jobs", "example.test"),
        "first"
    );
}

proptest! {
    #[test]
    fn longest_matching_prefix_wins(extra in "[a-z]{1,20}") {
        let short = route("short", Some("/api"));
        let long_prefix = format!("/api/{extra}");
        let long = route("long", Some(&long_prefix));
        let uri = format!("{long_prefix}/resource");

        prop_assert_eq!(matched_name(vec![short, long], &uri, "example.test"), "long");
    }
}
