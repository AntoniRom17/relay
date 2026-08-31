use std::{
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use relay_config::CircuitBreakerConfig;
use thiserror::Error;

/// Concurrency-safe circuit breaker with explicit closed, open, and half-open states.
#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    config: BreakerConfig,
    state: Mutex<State>,
}

#[derive(Debug, Clone, Copy)]
struct BreakerConfig {
    failure_threshold: u32,
    recovery_timeout: Duration,
    half_open_probe_limit: u32,
    success_threshold: u32,
}

#[derive(Debug)]
enum State {
    Closed { consecutive_failures: u32 },
    Open { opened_at: Instant },
    HalfOpen { in_flight: u32, successes: u32 },
}

impl CircuitBreaker {
    /// Creates a closed circuit breaker from validated configuration.
    pub fn new(config: &CircuitBreakerConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                config: BreakerConfig {
                    failure_threshold: config.failure_threshold,
                    recovery_timeout: Duration::from_millis(config.recovery_timeout_ms),
                    half_open_probe_limit: config.half_open_probe_limit,
                    success_threshold: config.success_threshold,
                },
                state: Mutex::new(State::Closed {
                    consecutive_failures: 0,
                }),
            }),
        }
    }

    /// Attempts to admit an upstream request through the circuit.
    pub fn try_acquire(&self) -> Result<CircuitPermit, CircuitOpen> {
        let now = Instant::now();
        let mut state = self.lock_state();
        if let State::Open { opened_at } = *state
            && now.duration_since(opened_at) >= self.inner.config.recovery_timeout
        {
            *state = State::HalfOpen {
                in_flight: 0,
                successes: 0,
            };
        }

        match &mut *state {
            State::Closed { .. } => {}
            State::Open { .. } => return Err(CircuitOpen),
            State::HalfOpen { in_flight, .. }
                if *in_flight >= self.inner.config.half_open_probe_limit =>
            {
                return Err(CircuitOpen);
            }
            State::HalfOpen { in_flight, .. } => *in_flight += 1,
        }
        drop(state);

        Ok(CircuitPermit {
            breaker: self.clone(),
            resolved: false,
        })
    }

    /// Returns a lock-consistent state snapshot.
    pub fn snapshot(&self) -> BreakerSnapshot {
        match &*self.lock_state() {
            State::Closed {
                consecutive_failures,
            } => BreakerSnapshot {
                state: CircuitState::Closed,
                consecutive_failures: *consecutive_failures,
                half_open_in_flight: 0,
                half_open_successes: 0,
            },
            State::Open { .. } => BreakerSnapshot {
                state: CircuitState::Open,
                consecutive_failures: self.inner.config.failure_threshold,
                half_open_in_flight: 0,
                half_open_successes: 0,
            },
            State::HalfOpen {
                in_flight,
                successes,
            } => BreakerSnapshot {
                state: CircuitState::HalfOpen,
                consecutive_failures: 0,
                half_open_in_flight: *in_flight,
                half_open_successes: *successes,
            },
        }
    }

    fn record_success(&self) {
        let mut state = self.lock_state();
        match &mut *state {
            State::Closed {
                consecutive_failures,
            } => *consecutive_failures = 0,
            State::Open { .. } => {}
            State::HalfOpen {
                in_flight,
                successes,
            } => {
                *in_flight = in_flight.saturating_sub(1);
                *successes = successes.saturating_add(1);
                if *successes >= self.inner.config.success_threshold {
                    *state = State::Closed {
                        consecutive_failures: 0,
                    };
                }
            }
        }
    }

    fn record_failure(&self) {
        let mut state = self.lock_state();
        match &mut *state {
            State::Closed {
                consecutive_failures,
            } => {
                *consecutive_failures = consecutive_failures.saturating_add(1);
                if *consecutive_failures >= self.inner.config.failure_threshold {
                    *state = State::Open {
                        opened_at: Instant::now(),
                    };
                }
            }
            State::HalfOpen { .. } => {
                *state = State::Open {
                    opened_at: Instant::now(),
                };
            }
            State::Open { .. } => {}
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Permit representing one admitted circuit-breaker operation.
#[derive(Debug)]
pub struct CircuitPermit {
    breaker: CircuitBreaker,
    resolved: bool,
}

impl CircuitPermit {
    /// Records a successful operation and consumes the permit.
    pub fn success(mut self) {
        self.breaker.record_success();
        self.resolved = true;
    }

    /// Records a failed operation and consumes the permit.
    pub fn failure(mut self) {
        self.breaker.record_failure();
        self.resolved = true;
    }
}

impl Drop for CircuitPermit {
    fn drop(&mut self) {
        if !self.resolved {
            self.breaker.record_failure();
        }
    }
}

/// The externally observable circuit-breaker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal requests are admitted and failures are counted.
    Closed,
    /// Requests are rejected until the recovery timeout expires.
    Open,
    /// A bounded number of recovery probes are admitted.
    HalfOpen,
}

impl CircuitState {
    /// Returns the stable integer used by Relay's Prometheus gauge.
    pub const fn metric_value(self) -> i64 {
        match self {
            Self::Closed => 0,
            Self::HalfOpen => 1,
            Self::Open => 2,
        }
    }
}

/// Point-in-time circuit-breaker state and counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreakerSnapshot {
    /// Current state.
    pub state: CircuitState,
    /// Consecutive failures observed while closed.
    pub consecutive_failures: u32,
    /// Currently admitted half-open probes.
    pub half_open_in_flight: u32,
    /// Successful probes in the current recovery attempt.
    pub half_open_successes: u32,
}

/// Request rejection caused by an open circuit.
#[derive(Debug, Clone, Copy, Error, PartialEq, Eq)]
#[error("upstream circuit is open")]
pub struct CircuitOpen;

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    fn breaker() -> CircuitBreaker {
        CircuitBreaker::new(&CircuitBreakerConfig {
            failure_threshold: 2,
            recovery_timeout_ms: 5,
            half_open_probe_limit: 2,
            success_threshold: 2,
        })
    }

    #[test]
    fn failures_open_then_successful_probes_close_the_circuit() {
        let breaker = breaker();
        breaker.try_acquire().expect("closed circuit").failure();
        assert_eq!(breaker.snapshot().state, CircuitState::Closed);
        breaker.try_acquire().expect("closed circuit").failure();
        assert_eq!(breaker.snapshot().state, CircuitState::Open);
        assert_eq!(breaker.try_acquire().unwrap_err(), CircuitOpen);

        thread::sleep(Duration::from_millis(10));
        let first = breaker.try_acquire().expect("first recovery probe");
        let second = breaker.try_acquire().expect("second recovery probe");
        assert_eq!(breaker.snapshot().state, CircuitState::HalfOpen);
        assert_eq!(breaker.try_acquire().unwrap_err(), CircuitOpen);

        first.success();
        assert_eq!(breaker.snapshot().state, CircuitState::HalfOpen);
        second.success();
        assert_eq!(breaker.snapshot().state, CircuitState::Closed);
    }

    #[test]
    fn a_failed_half_open_probe_reopens_the_circuit() {
        let breaker = breaker();
        breaker.try_acquire().expect("closed circuit").failure();
        breaker.try_acquire().expect("closed circuit").failure();
        thread::sleep(Duration::from_millis(10));

        breaker.try_acquire().expect("recovery probe").failure();

        assert_eq!(breaker.snapshot().state, CircuitState::Open);
    }

    #[test]
    fn cancelled_operation_is_conservatively_recorded_as_failure() {
        let breaker = breaker();

        drop(breaker.try_acquire().expect("closed circuit"));

        assert_eq!(breaker.snapshot().consecutive_failures, 1);
    }
}
