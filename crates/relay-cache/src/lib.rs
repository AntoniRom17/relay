//! Bounded, expiry-aware storage used by Relay's streaming cache layer.

mod memory;

pub use memory::{CacheRegistry, CachedResponse, ResponseCache};
