use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use relay_config::HealthCheckConfig;
use tokio::{task::JoinHandle, time::MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use url::Url;

use crate::{Endpoint, Pool};

/// Transport-independent operation used by active health checking.
#[async_trait]
pub trait HealthProbe: Send + Sync + 'static {
    /// Returns whether an endpoint produced the configured healthy response.
    async fn probe(
        &self,
        endpoint: &Url,
        path: &str,
        timeout: Duration,
        accepted_status: std::ops::RangeInclusive<u16>,
    ) -> bool;
}

/// Starts a cancellable active health-check loop for one upstream pool.
pub fn spawn_health_checks(
    pool: Arc<Pool>,
    config: HealthCheckConfig,
    probe: Arc<dyn HealthProbe>,
    shutdown: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(config.interval_ms));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => break,
                _ = interval.tick() => {
                    run_round(&pool, &config, Arc::clone(&probe), &shutdown).await;
                }
            }
        }
    })
}

async fn run_round(
    pool: &Pool,
    config: &HealthCheckConfig,
    probe: Arc<dyn HealthProbe>,
    shutdown: &CancellationToken,
) {
    let mut probes = tokio::task::JoinSet::new();
    for endpoint in pool.endpoints() {
        let endpoint = Arc::clone(endpoint);
        let probe = Arc::clone(&probe);
        let path = config.path.clone();
        let timeout = Duration::from_millis(config.timeout_ms);
        let accepted_status = config.expected_status_min..=config.expected_status_max;
        probes.spawn(async move {
            let success = probe
                .probe(endpoint.url(), &path, timeout, accepted_status)
                .await;
            (endpoint, success)
        });
    }

    while !probes.is_empty() {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                probes.abort_all();
                break;
            }
            result = probes.join_next(), if !probes.is_empty() => {
                match result {
                    Some(Ok((endpoint, success))) => {
                        record_result(pool.name(), &endpoint, config, success);
                    }
                    Some(Err(error)) => warn!(pool = pool.name(), %error, "health probe task failed"),
                    None => break,
                }
            }
        }
    }
}

fn record_result(pool: &str, endpoint: &Endpoint, config: &HealthCheckConfig, success: bool) {
    let was_healthy = endpoint.is_healthy();
    let snapshot = endpoint.record_health_result(
        success,
        config.healthy_threshold,
        config.unhealthy_threshold,
    );
    if snapshot.healthy != was_healthy {
        info!(
            pool,
            endpoint = %endpoint.url(),
            healthy = snapshot.healthy,
            consecutive_successes = snapshot.consecutive_successes,
            consecutive_failures = snapshot.consecutive_failures,
            "upstream health state changed"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use relay_config::{CircuitBreakerConfig, EndpointConfig, LoadBalancingPolicy, UpstreamConfig};

    use super::*;

    struct ControllableProbe(AtomicBool);

    #[async_trait]
    impl HealthProbe for ControllableProbe {
        async fn probe(
            &self,
            _endpoint: &Url,
            _path: &str,
            _timeout: Duration,
            _accepted_status: std::ops::RangeInclusive<u16>,
        ) -> bool {
            self.0.load(Ordering::Acquire)
        }
    }

    fn pool() -> Arc<Pool> {
        Arc::new(Pool::from_config(
            "api",
            &UpstreamConfig {
                endpoints: vec![EndpointConfig {
                    url: "http://127.0.0.1:9001".parse().expect("valid URL"),
                    weight: 1,
                }],
                load_balancing: LoadBalancingPolicy::RoundRobin,
                health_check: None,
                circuit_breaker: CircuitBreakerConfig::default(),
                connect_timeout_ms: 100,
                concurrency_limit: 8,
            },
        ))
    }

    fn health_config() -> HealthCheckConfig {
        HealthCheckConfig {
            path: "/health".to_owned(),
            interval_ms: 5,
            timeout_ms: 5,
            expected_status_min: 200,
            expected_status_max: 299,
            healthy_threshold: 2,
            unhealthy_threshold: 2,
        }
    }

    async fn wait_for_health(endpoint: &Endpoint, expected: bool) {
        tokio::time::timeout(Duration::from_millis(250), async {
            while endpoint.is_healthy() != expected {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("health state should transition");
    }

    #[tokio::test]
    async fn runner_ejects_and_recovers_endpoint_then_stops() {
        let pool = pool();
        let probe = Arc::new(ControllableProbe(AtomicBool::new(false)));
        let shutdown = CancellationToken::new();
        let task = spawn_health_checks(
            Arc::clone(&pool),
            health_config(),
            Arc::clone(&probe) as Arc<dyn HealthProbe>,
            shutdown.clone(),
        );

        wait_for_health(&pool.endpoints()[0], false).await;
        probe.0.store(true, Ordering::Release);
        wait_for_health(&pool.endpoints()[0], true).await;

        shutdown.cancel();
        tokio::time::timeout(Duration::from_millis(100), task)
            .await
            .expect("health task should stop promptly")
            .expect("health task should not panic");
    }
}
