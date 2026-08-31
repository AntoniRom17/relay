//! Upstream pools, endpoint lifecycle, scheduling, and reliability state.

mod endpoint;

pub use endpoint::{Endpoint, EndpointLease, HealthSnapshot};
