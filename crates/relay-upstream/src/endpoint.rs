use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
};

use url::Url;

/// Concurrent runtime state for a configured backend endpoint.
#[derive(Debug)]
pub struct Endpoint {
    url: Url,
    weight: u32,
    healthy: AtomicBool,
    active: AtomicUsize,
    consecutive_successes: AtomicU32,
    consecutive_failures: AtomicU32,
}

impl Endpoint {
    /// Creates an initially healthy endpoint.
    pub fn new(url: Url, weight: u32) -> Self {
        Self {
            url,
            weight,
            healthy: AtomicBool::new(true),
            active: AtomicUsize::new(0),
            consecutive_successes: AtomicU32::new(0),
            consecutive_failures: AtomicU32::new(0),
        }
    }

    /// Returns the configured origin URL.
    pub const fn url(&self) -> &Url {
        &self.url
    }

    /// Returns the relative scheduling weight.
    pub const fn weight(&self) -> u32 {
        self.weight
    }

    /// Returns whether the endpoint is currently eligible for traffic.
    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    /// Returns the number of currently active forwarding attempts.
    pub fn active_requests(&self) -> usize {
        self.active.load(Ordering::Relaxed)
    }

    /// Atomically records an active health-check result and returns the new snapshot.
    pub fn record_health_result(
        &self,
        success: bool,
        healthy_threshold: u32,
        unhealthy_threshold: u32,
    ) -> HealthSnapshot {
        if success {
            self.consecutive_failures.store(0, Ordering::Relaxed);
            let successes = self
                .consecutive_successes
                .fetch_add(1, Ordering::AcqRel)
                .saturating_add(1);
            if successes >= healthy_threshold {
                self.healthy.store(true, Ordering::Release);
            }
        } else {
            self.consecutive_successes.store(0, Ordering::Relaxed);
            let failures = self
                .consecutive_failures
                .fetch_add(1, Ordering::AcqRel)
                .saturating_add(1);
            if failures >= unhealthy_threshold {
                self.healthy.store(false, Ordering::Release);
            }
        }
        self.health_snapshot()
    }

    /// Records a passive transport failure without immediately ejecting a healthy endpoint.
    pub fn record_passive_failure(&self, unhealthy_threshold: u32) -> HealthSnapshot {
        self.record_health_result(false, u32::MAX, unhealthy_threshold)
    }

    /// Returns current health counters without locking.
    pub fn health_snapshot(&self) -> HealthSnapshot {
        HealthSnapshot {
            healthy: self.is_healthy(),
            consecutive_successes: self.consecutive_successes.load(Ordering::Acquire),
            consecutive_failures: self.consecutive_failures.load(Ordering::Acquire),
        }
    }

    pub(crate) fn acquire(endpoint: &Arc<Self>) -> EndpointLease {
        endpoint.active.fetch_add(1, Ordering::AcqRel);
        EndpointLease {
            endpoint: Arc::clone(endpoint),
        }
    }
}

/// Point-in-time endpoint health state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HealthSnapshot {
    /// Whether the endpoint is currently eligible for traffic.
    pub healthy: bool,
    /// Consecutive successful health results.
    pub consecutive_successes: u32,
    /// Consecutive failed health results.
    pub consecutive_failures: u32,
}

/// RAII guard accounting for an in-flight request on an endpoint.
#[derive(Debug)]
pub struct EndpointLease {
    endpoint: Arc<Endpoint>,
}

impl EndpointLease {
    /// Returns the selected endpoint.
    pub const fn endpoint(&self) -> &Arc<Endpoint> {
        &self.endpoint
    }
}

impl Drop for EndpointLease {
    fn drop(&mut self) {
        self.endpoint.active.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint() -> Arc<Endpoint> {
        Arc::new(Endpoint::new(
            "http://127.0.0.1:9001".parse().expect("valid URL"),
            1,
        ))
    }

    #[test]
    fn health_transitions_require_consecutive_results() {
        let endpoint = endpoint();

        assert!(endpoint.record_health_result(false, 2, 2).healthy);
        assert!(!endpoint.record_health_result(false, 2, 2).healthy);
        assert!(!endpoint.record_health_result(true, 2, 2).healthy);
        assert!(endpoint.record_health_result(true, 2, 2).healthy);
    }

    #[test]
    fn lease_accounts_for_in_flight_requests() {
        let endpoint = endpoint();
        let first = Endpoint::acquire(&endpoint);
        let second = Endpoint::acquire(&endpoint);
        assert_eq!(endpoint.active_requests(), 2);

        drop(first);
        drop(second);
        assert_eq!(endpoint.active_requests(), 0);
    }
}
