//! Sharded local and atomic distributed rate-limit policies.

mod local;

pub use local::{LimitDecision, LocalLimitError, LocalRateLimiter};
