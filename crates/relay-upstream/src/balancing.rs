use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use crate::{Endpoint, EndpointLease};

/// Selects a healthy endpoint for one forwarding attempt.
pub trait LoadBalancer: Send + Sync {
    /// Returns a lease on the selected endpoint, or `None` when none are healthy.
    fn select(&self, endpoints: &[Arc<Endpoint>]) -> Option<EndpointLease>;
}

/// Lock-free round-robin selection with health-aware skipping.
#[derive(Debug, Default)]
pub struct RoundRobin {
    cursor: AtomicUsize,
}

impl RoundRobin {
    /// Creates a scheduler starting at the first endpoint.
    pub const fn new() -> Self {
        Self {
            cursor: AtomicUsize::new(0),
        }
    }
}

impl LoadBalancer for RoundRobin {
    fn select(&self, endpoints: &[Arc<Endpoint>]) -> Option<EndpointLease> {
        if endpoints.is_empty() {
            return None;
        }
        let start = self.cursor.fetch_add(1, Ordering::Relaxed) % endpoints.len();
        healthy_from(endpoints, start).map(Endpoint::acquire)
    }
}

/// Round-robin selection over a compact, interleaved weighted schedule.
#[derive(Debug)]
pub struct WeightedRoundRobin {
    cursor: AtomicUsize,
    schedule: Box<[usize]>,
}

impl WeightedRoundRobin {
    /// Builds a schedule whose frequency is proportional to endpoint weights.
    pub fn new(endpoints: &[Arc<Endpoint>]) -> Self {
        let divisor = endpoints
            .iter()
            .map(|endpoint| endpoint.weight())
            .reduce(greatest_common_divisor)
            .unwrap_or(1);
        let normalized = endpoints
            .iter()
            .map(|endpoint| endpoint.weight() / divisor)
            .collect::<Vec<_>>();
        let rounds = normalized.iter().copied().max().unwrap_or(0);
        let mut schedule = Vec::new();
        for round in 0..rounds {
            for (index, weight) in normalized.iter().copied().enumerate() {
                if weight > round {
                    schedule.push(index);
                }
            }
        }
        Self {
            cursor: AtomicUsize::new(0),
            schedule: schedule.into_boxed_slice(),
        }
    }
}

impl LoadBalancer for WeightedRoundRobin {
    fn select(&self, endpoints: &[Arc<Endpoint>]) -> Option<EndpointLease> {
        if self.schedule.is_empty() {
            return None;
        }
        let start = self.cursor.fetch_add(1, Ordering::Relaxed) % self.schedule.len();
        (0..self.schedule.len())
            .map(|offset| self.schedule[(start + offset) % self.schedule.len()])
            .filter_map(|index| endpoints.get(index))
            .find(|endpoint| endpoint.is_healthy())
            .map(Endpoint::acquire)
    }
}

/// Selects the healthy endpoint with the fewest currently active requests.
#[derive(Debug, Default)]
pub struct LeastConnections;

impl LoadBalancer for LeastConnections {
    fn select(&self, endpoints: &[Arc<Endpoint>]) -> Option<EndpointLease> {
        endpoints
            .iter()
            .filter(|endpoint| endpoint.is_healthy())
            .min_by_key(|endpoint| endpoint.active_requests())
            .map(Endpoint::acquire)
    }
}

fn healthy_from(endpoints: &[Arc<Endpoint>], start: usize) -> Option<&Arc<Endpoint>> {
    (0..endpoints.len())
        .map(|offset| &endpoints[(start + offset) % endpoints.len()])
        .find(|endpoint| endpoint.is_healthy())
}

const fn greatest_common_divisor(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(port: u16, weight: u32) -> Arc<Endpoint> {
        Arc::new(Endpoint::new(
            format!("http://127.0.0.1:{port}")
                .parse()
                .expect("valid URL"),
            weight,
        ))
    }

    #[test]
    fn round_robin_rotates_and_skips_unhealthy_endpoints() {
        let endpoints = vec![endpoint(9001, 1), endpoint(9002, 1), endpoint(9003, 1)];
        endpoints[1].record_health_result(false, 1, 1);
        let policy = RoundRobin::new();

        let selected = (0..4)
            .map(|_| {
                policy
                    .select(&endpoints)
                    .expect("healthy endpoint")
                    .endpoint()
                    .url()
                    .port()
                    .expect("configured port")
            })
            .collect::<Vec<_>>();

        assert_eq!(selected, vec![9001, 9003, 9003, 9001]);
    }

    #[test]
    fn weighted_round_robin_honors_relative_frequency() {
        let endpoints = vec![endpoint(9001, 3), endpoint(9002, 1)];
        let policy = WeightedRoundRobin::new(&endpoints);
        let mut first = 0;
        let mut second = 0;

        for _ in 0..40 {
            let lease = policy.select(&endpoints).expect("healthy endpoint");
            match lease.endpoint().url().port() {
                Some(9001) => first += 1,
                Some(9002) => second += 1,
                port => panic!("unexpected port {port:?}"),
            }
        }

        assert_eq!((first, second), (30, 10));
    }

    #[test]
    fn least_connections_uses_raii_accounting() {
        let endpoints = vec![endpoint(9001, 1), endpoint(9002, 1)];
        let busy = Endpoint::acquire(&endpoints[0]);
        let policy = LeastConnections;

        let selected = policy.select(&endpoints).expect("healthy endpoint");

        assert_eq!(selected.endpoint().url().port(), Some(9002));
        drop(busy);
        drop(selected);
        assert!(
            endpoints
                .iter()
                .all(|endpoint| endpoint.active_requests() == 0)
        );
    }
}
