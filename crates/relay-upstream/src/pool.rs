use std::sync::Arc;

use relay_config::{LoadBalancingPolicy, UpstreamConfig};
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{
    Endpoint, EndpointLease, LeastConnections, LoadBalancer, RoundRobin, WeightedRoundRobin,
};

/// A concurrency-bounded pool of backend endpoints.
pub struct Pool {
    name: Arc<str>,
    endpoints: Arc<[Arc<Endpoint>]>,
    balancer: Arc<dyn LoadBalancer>,
    concurrency: Arc<Semaphore>,
}

impl Pool {
    /// Constructs runtime pool state from validated configuration.
    pub fn from_config(name: impl Into<Arc<str>>, config: &UpstreamConfig) -> Self {
        let endpoints: Arc<[Arc<Endpoint>]> = config
            .endpoints
            .iter()
            .map(|endpoint| Arc::new(Endpoint::new(endpoint.url.clone(), endpoint.weight)))
            .collect::<Vec<_>>()
            .into();
        let balancer: Arc<dyn LoadBalancer> = match config.load_balancing {
            LoadBalancingPolicy::RoundRobin => Arc::new(RoundRobin::new()),
            LoadBalancingPolicy::WeightedRoundRobin => {
                Arc::new(WeightedRoundRobin::new(&endpoints))
            }
            LoadBalancingPolicy::LeastConnections => Arc::new(LeastConnections),
        };
        Self {
            name: name.into(),
            endpoints,
            balancer,
            concurrency: Arc::new(Semaphore::new(config.concurrency_limit)),
        }
    }

    /// Returns the stable configured pool name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the pool's runtime endpoints.
    pub fn endpoints(&self) -> &[Arc<Endpoint>] {
        &self.endpoints
    }

    /// Immediately selects an endpoint without creating an unbounded wait queue.
    pub fn try_select(&self) -> Result<UpstreamLease, PoolSelectionError> {
        let permit = Arc::clone(&self.concurrency)
            .try_acquire_owned()
            .map_err(|_| PoolSelectionError::Overloaded)?;
        let endpoint = self
            .balancer
            .select(&self.endpoints)
            .ok_or(PoolSelectionError::NoHealthyEndpoint)?;
        Ok(UpstreamLease {
            endpoint,
            _pool_permit: permit,
        })
    }

    /// Returns a point-in-time snapshot suitable for administration APIs.
    pub fn snapshot(&self) -> PoolSnapshot {
        PoolSnapshot {
            name: self.name.to_string(),
            available_capacity: self.concurrency.available_permits(),
            endpoints: self
                .endpoints
                .iter()
                .map(|endpoint| EndpointSnapshot {
                    url: endpoint.url().to_string(),
                    healthy: endpoint.is_healthy(),
                    active_requests: endpoint.active_requests(),
                })
                .collect(),
        }
    }
}

impl std::fmt::Debug for Pool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Pool")
            .field("name", &self.name)
            .field("endpoints", &self.endpoints)
            .field("available_capacity", &self.concurrency.available_permits())
            .finish_non_exhaustive()
    }
}

/// Failure to admit a forwarding attempt to an upstream pool.
#[derive(Debug, Clone, Copy, Error, PartialEq, Eq)]
pub enum PoolSelectionError {
    /// Every endpoint has been marked unhealthy.
    #[error("no healthy endpoint is available")]
    NoHealthyEndpoint,
    /// The pool's bounded concurrency budget is exhausted.
    #[error("upstream concurrency limit is exhausted")]
    Overloaded,
}

/// RAII guard accounting for both endpoint and pool concurrency.
#[derive(Debug)]
pub struct UpstreamLease {
    endpoint: EndpointLease,
    _pool_permit: OwnedSemaphorePermit,
}

impl UpstreamLease {
    /// Returns the selected endpoint.
    pub fn endpoint(&self) -> &Arc<Endpoint> {
        self.endpoint.endpoint()
    }
}

/// Administrative snapshot of a pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolSnapshot {
    /// Pool name.
    pub name: String,
    /// Remaining pool concurrency permits.
    pub available_capacity: usize,
    /// Endpoint health and activity.
    pub endpoints: Vec<EndpointSnapshot>,
}

/// Administrative snapshot of an endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointSnapshot {
    /// Configured origin URL.
    pub url: String,
    /// Whether normal traffic is eligible.
    pub healthy: bool,
    /// Current forwarding attempts.
    pub active_requests: usize,
}

#[cfg(test)]
mod tests {
    use relay_config::{CircuitBreakerConfig, EndpointConfig};

    use super::*;

    fn config(concurrency_limit: usize) -> UpstreamConfig {
        UpstreamConfig {
            endpoints: vec![EndpointConfig {
                url: "http://127.0.0.1:9001".parse().expect("valid URL"),
                weight: 1,
            }],
            load_balancing: LoadBalancingPolicy::RoundRobin,
            health_check: None,
            circuit_breaker: CircuitBreakerConfig::default(),
            connect_timeout_ms: 1_000,
            concurrency_limit,
        }
    }

    #[test]
    fn exhausted_capacity_is_shed_without_queueing() {
        let pool = Pool::from_config("api", &config(1));
        let lease = pool.try_select().expect("first attempt should be admitted");

        assert_eq!(
            pool.try_select().unwrap_err(),
            PoolSelectionError::Overloaded
        );
        assert_eq!(pool.snapshot().available_capacity, 0);

        drop(lease);
        assert!(pool.try_select().is_ok());
    }

    #[test]
    fn all_unhealthy_endpoints_are_reported_distinctly() {
        let pool = Pool::from_config("api", &config(8));
        pool.endpoints()[0].record_health_result(false, 1, 1);

        assert_eq!(
            pool.try_select().unwrap_err(),
            PoolSelectionError::NoHealthyEndpoint
        );
        assert_eq!(pool.snapshot().available_capacity, 8);
    }
}
