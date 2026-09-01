use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use dashmap::{DashMap, mapref::entry::Entry};
use relay_config::RateLimitConfig;
use thiserror::Error;

/// A sharded token-bucket limiter with bounded key cardinality.
#[derive(Debug)]
pub struct LocalRateLimiter {
    buckets: DashMap<String, Bucket>,
    max_keys: usize,
    keys: AtomicUsize,
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    updated_at: Instant,
}

impl LocalRateLimiter {
    /// Creates a limiter that refuses to allocate beyond `max_keys` buckets.
    pub fn new(max_keys: usize) -> Self {
        Self {
            buckets: DashMap::new(),
            max_keys,
            keys: AtomicUsize::new(0),
        }
    }

    /// Consumes one token for `key` using the supplied route policy.
    ///
    /// # Errors
    ///
    /// Returns [`LocalLimitError::CapacityExceeded`] rather than allocating an unbounded number of
    /// independent buckets.
    pub fn check(
        &self,
        key: impl Into<String>,
        config: &RateLimitConfig,
    ) -> Result<LimitDecision, LocalLimitError> {
        self.check_at(key.into(), config, Instant::now())
    }

    /// Removes buckets that have not been touched within `max_idle`.
    pub fn prune_idle(&self, max_idle: Duration) -> usize {
        let now = Instant::now();
        let removed = AtomicUsize::new(0);
        self.buckets.retain(|_, bucket| {
            let keep = now.duration_since(bucket.updated_at) <= max_idle;
            if !keep {
                removed.fetch_add(1, Ordering::Relaxed);
            }
            keep
        });
        let removed = removed.load(Ordering::Relaxed);
        self.keys.fetch_sub(removed, Ordering::AcqRel);
        removed
    }

    /// Returns the number of independently tracked keys.
    pub fn tracked_keys(&self) -> usize {
        self.buckets.len()
    }

    fn check_at(
        &self,
        key: String,
        config: &RateLimitConfig,
        now: Instant,
    ) -> Result<LimitDecision, LocalLimitError> {
        let capacity = f64::from(config.burst_capacity);
        if let Some(mut bucket) = self.buckets.get_mut(&key) {
            return Ok(apply_bucket(&mut bucket, config, now));
        }
        self.keys
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |keys| {
                (keys < self.max_keys).then_some(keys + 1)
            })
            .map_err(|_| LocalLimitError::CapacityExceeded {
                max_keys: self.max_keys,
            })?;
        let mut bucket = match self.buckets.entry(key) {
            Entry::Occupied(entry) => {
                self.keys.fetch_sub(1, Ordering::AcqRel);
                entry.into_ref()
            }
            Entry::Vacant(entry) => entry.insert(Bucket {
                tokens: capacity,
                updated_at: now,
            }),
        };
        Ok(apply_bucket(&mut bucket, config, now))
    }
}

fn apply_bucket(bucket: &mut Bucket, config: &RateLimitConfig, now: Instant) -> LimitDecision {
    let rate = f64::from(config.requests_per_second);
    let capacity = f64::from(config.burst_capacity);
    let elapsed = now.duration_since(bucket.updated_at).as_secs_f64();
    bucket.tokens = (bucket.tokens + elapsed * rate).min(capacity);
    bucket.updated_at = now;
    if bucket.tokens >= 1.0 {
        bucket.tokens -= 1.0;
        LimitDecision {
            allowed: true,
            remaining: whole_tokens(bucket.tokens),
            retry_after: Duration::ZERO,
        }
    } else {
        let missing = 1.0 - bucket.tokens;
        LimitDecision {
            allowed: false,
            remaining: 0,
            retry_after: Duration::from_secs_f64(missing / rate),
        }
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn whole_tokens(tokens: f64) -> u32 {
    // Token state is clamped to a u32-derived burst capacity and cannot be negative here.
    debug_assert!(tokens >= 0.0 && tokens <= f64::from(u32::MAX));
    tokens.floor() as u32
}

/// Result of applying a token-bucket policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitDecision {
    /// Whether the request may proceed.
    pub allowed: bool,
    /// Whole tokens remaining after the decision.
    pub remaining: u32,
    /// Minimum estimated wait before a rejected request can succeed.
    pub retry_after: Duration,
}

/// Local limiter could not safely allocate policy state.
#[derive(Debug, Clone, Copy, Error, PartialEq, Eq)]
pub enum LocalLimitError {
    /// The configured key-cardinality bound has been reached.
    #[error("rate limiter key capacity of {max_keys} is exhausted")]
    CapacityExceeded {
        /// Maximum number of distinct keys.
        max_keys: usize,
    },
}

#[cfg(test)]
mod tests {
    use relay_config::RateLimitKey;

    use super::*;

    fn config() -> RateLimitConfig {
        RateLimitConfig {
            requests_per_second: 10,
            burst_capacity: 2,
            key: RateLimitKey::ClientIp,
            distributed: false,
        }
    }

    #[test]
    fn bucket_bursts_then_refills_by_elapsed_time() {
        let limiter = LocalRateLimiter::new(10);
        let config = config();
        let start = Instant::now();

        assert!(
            limiter
                .check_at("client".to_owned(), &config, start)
                .unwrap()
                .allowed
        );
        assert!(
            limiter
                .check_at("client".to_owned(), &config, start)
                .unwrap()
                .allowed
        );
        let rejected = limiter
            .check_at("client".to_owned(), &config, start)
            .unwrap();
        assert!(!rejected.allowed);
        assert_eq!(rejected.retry_after, Duration::from_millis(100));
        assert!(
            limiter
                .check_at(
                    "client".to_owned(),
                    &config,
                    start + Duration::from_millis(100)
                )
                .unwrap()
                .allowed
        );
    }

    #[test]
    fn cardinality_is_bounded_and_idle_state_can_be_pruned() {
        let limiter = LocalRateLimiter::new(1);
        let config = config();
        limiter.check("first", &config).expect("first key fits");

        assert_eq!(
            limiter.check("second", &config).unwrap_err(),
            LocalLimitError::CapacityExceeded { max_keys: 1 }
        );
        assert_eq!(limiter.prune_idle(Duration::ZERO), 1);
        assert!(limiter.check("second", &config).is_ok());
    }
}
