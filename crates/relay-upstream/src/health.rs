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

    loop {
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
            else => break,
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
