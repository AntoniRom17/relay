use std::{sync::Arc, time::Duration};

use redis::{Script, aio::ConnectionManager};
use relay_config::RateLimitConfig;
use thiserror::Error;

use crate::LimitDecision;

const TOKEN_BUCKET_SCRIPT: &str = r"
local now_parts = redis.call('TIME')
local now = tonumber(now_parts[1]) + (tonumber(now_parts[2]) / 1000000)
local rate = tonumber(ARGV[1])
local capacity = tonumber(ARGV[2])
local values = redis.call('HMGET', KEYS[1], 'tokens', 'updated_at')
local tokens = tonumber(values[1]) or capacity
local updated_at = tonumber(values[2]) or now
tokens = math.min(capacity, tokens + math.max(0, now - updated_at) * rate)

local allowed = 0
local retry_ms = 0
if tokens >= 1 then
  tokens = tokens - 1
  allowed = 1
else
  retry_ms = math.ceil(((1 - tokens) / rate) * 1000)
end

redis.call('HSET', KEYS[1], 'tokens', tokens, 'updated_at', now)
local ttl_ms = math.max(1000, math.ceil((capacity / rate) * 2000))
redis.call('PEXPIRE', KEYS[1], ttl_ms)
return {allowed, math.floor(tokens), retry_ms}
";

/// Redis-backed token bucket using one server-time Lua transaction per decision.
#[derive(Clone)]
pub struct RedisRateLimiter {
    manager: ConnectionManager,
    prefix: Arc<str>,
}

impl RedisRateLimiter {
    /// Connects to Redis and creates a multiplexed reconnecting manager.
    ///
    /// # Errors
    ///
    /// Returns [`RedisLimitError`] when the URL is invalid or an initial Redis connection cannot
    /// be established.
    pub async fn connect(url: &str, prefix: impl Into<Arc<str>>) -> Result<Self, RedisLimitError> {
        let client = redis::Client::open(url).map_err(RedisLimitError::Redis)?;
        let manager = client
            .get_connection_manager()
            .await
            .map_err(RedisLimitError::Redis)?;
        Ok(Self {
            manager,
            prefix: prefix.into(),
        })
    }

    /// Atomically consumes one token for a route and non-secret identity key.
    ///
    /// # Errors
    ///
    /// Returns [`RedisLimitError`] when the atomic Redis script cannot be evaluated.
    pub async fn check(
        &self,
        route: &str,
        key: &str,
        config: &RateLimitConfig,
    ) -> Result<LimitDecision, RedisLimitError> {
        let redis_key = format!("{}:rate:{route}:{key}", self.prefix);
        let mut manager = self.manager.clone();
        let (allowed, remaining, retry_ms): (i64, i64, i64) = Script::new(TOKEN_BUCKET_SCRIPT)
            .key(redis_key)
            .arg(config.requests_per_second)
            .arg(config.burst_capacity)
            .invoke_async(&mut manager)
            .await
            .map_err(RedisLimitError::Redis)?;
        Ok(LimitDecision {
            allowed: allowed == 1,
            remaining: u32::try_from(remaining.max(0)).unwrap_or(u32::MAX),
            retry_after: Duration::from_millis(u64::try_from(retry_ms.max(0)).unwrap_or(u64::MAX)),
        })
    }
}

impl std::fmt::Debug for RedisRateLimiter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RedisRateLimiter")
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

/// Redis connectivity or atomic script failure.
#[derive(Debug, Error)]
pub enum RedisLimitError {
    /// Redis rejected or could not complete the operation.
    #[error("distributed rate limiter failed: {0}")]
    Redis(redis::RedisError),
}
