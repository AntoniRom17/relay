use std::sync::Arc;

use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, IntGauge, IntGaugeVec, Opts, Registry,
    TextEncoder,
};

/// Cloneable handles to Relay's Prometheus instruments and private registry.
#[derive(Clone)]
pub struct Metrics {
    inner: Arc<MetricsInner>,
}

struct MetricsInner {
    registry: Registry,
    requests: IntCounterVec,
    request_duration: HistogramVec,
    upstream_requests: IntCounterVec,
    upstream_duration: HistogramVec,
    upstream_failures: IntCounterVec,
    active_connections: IntGauge,
    rate_limit_rejections: IntCounterVec,
    retry_attempts: IntCounterVec,
    circuit_breaker_state: IntGaugeVec,
    upstream_health: IntGaugeVec,
    config_reload: IntCounterVec,
}

impl Metrics {
    /// Constructs and registers all gateway instruments in an isolated registry.
    pub fn new() -> Result<Self, prometheus::Error> {
        let registry = Registry::new();
        let requests = IntCounterVec::new(
            Opts::new("relay_requests_total", "Client requests processed by Relay"),
            &["route", "method", "status"],
        )?;
        let request_duration = HistogramVec::new(
            HistogramOpts::new(
                "relay_request_duration_seconds",
                "End-to-end client request latency",
            )
            .buckets(vec![
                0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0,
            ]),
            &["route"],
        )?;
        let upstream_requests = IntCounterVec::new(
            Opts::new(
                "relay_upstream_requests_total",
                "Forwarding attempts sent to upstream endpoints",
            ),
            &["pool", "endpoint", "status"],
        )?;
        let upstream_duration = HistogramVec::new(
            HistogramOpts::new(
                "relay_upstream_request_duration_seconds",
                "Latency of individual upstream attempts",
            )
            .buckets(vec![
                0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5,
            ]),
            &["pool", "endpoint"],
        )?;
        let upstream_failures = IntCounterVec::new(
            Opts::new(
                "relay_upstream_failures_total",
                "Upstream transport and policy failures",
            ),
            &["pool", "endpoint", "reason"],
        )?;
        let active_connections = IntGauge::new(
            "relay_active_connections",
            "Currently accepted client connections",
        )?;
        let rate_limit_rejections = IntCounterVec::new(
            Opts::new(
                "relay_rate_limit_rejections_total",
                "Requests rejected by a rate-limit policy",
            ),
            &["route", "scope"],
        )?;
        let retry_attempts = IntCounterVec::new(
            Opts::new(
                "relay_retry_attempts_total",
                "Additional forwarding attempts after the initial request",
            ),
            &["route", "reason"],
        )?;
        let circuit_breaker_state = IntGaugeVec::new(
            Opts::new(
                "relay_circuit_breaker_state",
                "Circuit state encoded as closed=0, half_open=1, open=2",
            ),
            &["pool"],
        )?;
        let upstream_health = IntGaugeVec::new(
            Opts::new(
                "relay_upstream_health",
                "Whether an endpoint is currently eligible for traffic",
            ),
            &["pool", "endpoint"],
        )?;
        let config_reload = IntCounterVec::new(
            Opts::new(
                "relay_config_reload_total",
                "Configuration reload attempts partitioned by outcome",
            ),
            &["outcome"],
        )?;

        for collector in [
            Box::new(requests.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(request_duration.clone()),
            Box::new(upstream_requests.clone()),
            Box::new(upstream_duration.clone()),
            Box::new(upstream_failures.clone()),
            Box::new(active_connections.clone()),
            Box::new(rate_limit_rejections.clone()),
            Box::new(retry_attempts.clone()),
            Box::new(circuit_breaker_state.clone()),
            Box::new(upstream_health.clone()),
            Box::new(config_reload.clone()),
        ] {
            registry.register(collector)?;
        }

        Ok(Self {
            inner: Arc::new(MetricsInner {
                registry,
                requests,
                request_duration,
                upstream_requests,
                upstream_duration,
                upstream_failures,
                active_connections,
                rate_limit_rejections,
                retry_attempts,
                circuit_breaker_state,
                upstream_health,
                config_reload,
            }),
        })
    }

    /// Records a completed client request.
    pub fn observe_request(&self, route: &str, method: &str, status: u16, seconds: f64) {
        self.inner
            .requests
            .with_label_values(&[route, method, &status.to_string()])
            .inc();
        self.inner
            .request_duration
            .with_label_values(&[route])
            .observe(seconds);
    }

    /// Records a completed upstream attempt.
    pub fn observe_upstream(&self, pool: &str, endpoint: &str, status: u16, seconds: f64) {
        self.inner
            .upstream_requests
            .with_label_values(&[pool, endpoint, &status.to_string()])
            .inc();
        self.inner
            .upstream_duration
            .with_label_values(&[pool, endpoint])
            .observe(seconds);
    }

    /// Records a failed upstream attempt.
    pub fn upstream_failure(&self, pool: &str, endpoint: &str, reason: &str) {
        self.inner
            .upstream_failures
            .with_label_values(&[pool, endpoint, reason])
            .inc();
    }

    /// Increments the accepted client-connection gauge.
    pub fn connection_opened(&self) {
        self.inner.active_connections.inc();
    }

    /// Decrements the accepted client-connection gauge.
    pub fn connection_closed(&self) {
        self.inner.active_connections.dec();
    }

    /// Records a rate-limit rejection.
    pub fn rate_limited(&self, route: &str, scope: &str) {
        self.inner
            .rate_limit_rejections
            .with_label_values(&[route, scope])
            .inc();
    }

    /// Records an additional retry attempt.
    pub fn retry(&self, route: &str, reason: &str) {
        self.inner
            .retry_attempts
            .with_label_values(&[route, reason])
            .inc();
    }

    /// Sets a pool's encoded circuit state.
    pub fn set_circuit_state(&self, pool: &str, encoded_state: i64) {
        self.inner
            .circuit_breaker_state
            .with_label_values(&[pool])
            .set(encoded_state);
    }

    /// Sets whether an endpoint is eligible for traffic.
    pub fn set_upstream_health(&self, pool: &str, endpoint: &str, healthy: bool) {
        self.inner
            .upstream_health
            .with_label_values(&[pool, endpoint])
            .set(i64::from(healthy));
    }

    /// Records a configuration reload outcome.
    pub fn config_reload(&self, outcome: &str) {
        self.inner.config_reload.with_label_values(&[outcome]).inc();
    }

    /// Encodes the current registry using Prometheus' text exposition format.
    pub fn encode(&self) -> Result<Vec<u8>, prometheus::Error> {
        let families = self.inner.registry.gather();
        let mut output = Vec::new();
        TextEncoder::new().encode(&families, &mut output)?;
        Ok(output)
    }
}

impl std::fmt::Debug for Metrics {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Metrics").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_exposes_stable_metric_names_and_labels() {
        let metrics = Metrics::new().expect("metric descriptors should be valid");
        metrics.observe_request("api", "GET", 200, 0.012);
        metrics.set_upstream_health("primary", "http://127.0.0.1:9001", true);

        let encoded = String::from_utf8(metrics.encode().expect("encoding should succeed"))
            .expect("Prometheus output should be UTF-8");

        assert!(
            encoded.contains("relay_requests_total{method=\"GET\",route=\"api\",status=\"200\"} 1")
        );
        assert!(encoded.contains(
            "relay_upstream_health{endpoint=\"http://127.0.0.1:9001\",pool=\"primary\"} 1"
        ));
    }
}
