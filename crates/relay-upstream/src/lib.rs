//! Upstream pools, endpoint lifecycle, scheduling, and reliability state.

mod balancing;
mod circuit_breaker;
mod endpoint;
mod health;
mod pool;

pub use balancing::{LeastConnections, LoadBalancer, RoundRobin, WeightedRoundRobin};
pub use circuit_breaker::{
    BreakerSnapshot, CircuitBreaker, CircuitOpen, CircuitPermit, CircuitState,
};
pub use endpoint::{Endpoint, EndpointLease, HealthSnapshot};
pub use health::{HealthProbe, spawn_health_checks};
pub use pool::{EndpointSnapshot, Pool, PoolSelectionError, PoolSnapshot, UpstreamLease};
