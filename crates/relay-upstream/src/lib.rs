//! Upstream pools, endpoint lifecycle, scheduling, and reliability state.

mod balancing;
mod endpoint;

pub use balancing::{LeastConnections, LoadBalancer, RoundRobin, WeightedRoundRobin};
pub use endpoint::{Endpoint, EndpointLease, HealthSnapshot};
