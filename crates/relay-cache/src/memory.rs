use std::{
    sync::Arc,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use bytes::Bytes;
use dashmap::{DashMap, mapref::entry::Entry};
use http::{HeaderMap, StatusCode, Version};

/// Complete cacheable response representation without hop-by-hop metadata.
#[derive(Debug, Clone)]
pub struct CachedResponse {
    /// HTTP status.
    pub status: StatusCode,
    /// HTTP version observed from the upstream.
    pub version: Version,
    /// End-to-end response headers.
    pub headers: HeaderMap,
    /// Fully received response body.
    pub body: Bytes,
    /// Monotonic insertion time used to compute the `Age` header.
    pub stored_at: Instant,
    expires_at: Instant,
}

impl CachedResponse {
    /// Creates a response with a monotonic expiry deadline.
    pub fn new(
        status: StatusCode,
        version: Version,
        headers: HeaderMap,
        body: Bytes,
        ttl: Duration,
    ) -> Self {
        let stored_at = Instant::now();
        Self {
            status,
            version,
            headers,
            body,
            stored_at,
            expires_at: stored_at + ttl,
        }
    }

    fn is_fresh(&self, now: Instant) -> bool {
        now < self.expires_at
    }
}

/// Concurrent cache with a strict upper bound on distinct entries.
#[derive(Debug)]
pub struct ResponseCache {
    entries: DashMap<String, CachedResponse>,
    count: AtomicUsize,
    max_entries: usize,
}

#[derive(Debug, Clone)]
struct RouteCache {
    max_entries: usize,
    cache: Arc<ResponseCache>,
}

/// Lazily-created, per-route caches whose capacities follow the live configuration.
#[derive(Debug, Default)]
pub struct CacheRegistry {
    routes: DashMap<String, RouteCache>,
}

impl CacheRegistry {
    /// Returns a route cache with exactly the requested capacity.
    ///
    /// A capacity change replaces the namespace atomically for new requests. Existing
    /// response streams retain their previous cache until they complete.
    pub fn route(&self, route: &str, max_entries: usize) -> Arc<ResponseCache> {
        match self.routes.entry(route.to_owned()) {
            Entry::Occupied(mut entry) => {
                if entry.get().max_entries != max_entries {
                    entry.insert(RouteCache {
                        max_entries,
                        cache: Arc::new(ResponseCache::new(max_entries)),
                    });
                }
                Arc::clone(&entry.get().cache)
            }
            Entry::Vacant(entry) => Arc::clone(
                &entry
                    .insert(RouteCache {
                        max_entries,
                        cache: Arc::new(ResponseCache::new(max_entries)),
                    })
                    .cache,
            ),
        }
    }
}

impl ResponseCache {
    /// Creates a cache bounded to `max_entries`; zero disables insertion.
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: DashMap::new(),
            count: AtomicUsize::new(0),
            max_entries,
        }
    }

    /// Returns a fresh cached response, lazily evicting an expired entry.
    pub fn get(&self, key: &str) -> Option<CachedResponse> {
        let now = Instant::now();
        if let Some(entry) = self.entries.get(key) {
            if entry.is_fresh(now) {
                return Some(entry.clone());
            }
        }
        if self.entries.remove(key).is_some() {
            self.count.fetch_sub(1, Ordering::AcqRel);
        }
        None
    }

    /// Inserts a complete response, returning false when capacity is exhausted.
    pub fn insert(&self, key: String, response: CachedResponse) -> bool {
        if let Some(mut existing) = self.entries.get_mut(&key) {
            *existing = response;
            return true;
        }
        if self
            .count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < self.max_entries).then_some(count + 1)
            })
            .is_err()
        {
            return false;
        }
        match self.entries.entry(key) {
            Entry::Occupied(mut entry) => {
                self.count.fetch_sub(1, Ordering::AcqRel);
                entry.insert(response);
            }
            Entry::Vacant(entry) => {
                entry.insert(response);
            }
        }
        true
    }

    /// Removes expired entries and returns the number evicted.
    pub fn prune_expired(&self) -> usize {
        let now = Instant::now();
        let removed = AtomicUsize::new(0);
        self.entries.retain(|_, response| {
            let keep = response.is_fresh(now);
            if !keep {
                removed.fetch_add(1, Ordering::Relaxed);
            }
            keep
        });
        let removed = removed.load(Ordering::Relaxed);
        self.count.fetch_sub(removed, Ordering::AcqRel);
        removed
    }

    /// Returns the current entry count.
    pub fn len(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }

    /// Returns whether the cache currently contains no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(ttl: Duration) -> CachedResponse {
        CachedResponse::new(
            StatusCode::OK,
            Version::HTTP_11,
            HeaderMap::new(),
            Bytes::from_static(b"cached"),
            ttl,
        )
    }

    #[test]
    fn capacity_is_bounded_and_existing_entries_can_refresh() {
        let cache = ResponseCache::new(1);
        assert!(cache.insert("first".to_owned(), response(Duration::from_secs(10))));
        assert!(!cache.insert("second".to_owned(), response(Duration::from_secs(10))));
        assert!(cache.insert("first".to_owned(), response(Duration::from_secs(20))));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn expired_entries_are_lazily_removed() {
        let cache = ResponseCache::new(1);
        assert!(cache.insert("expired".to_owned(), response(Duration::ZERO)));

        assert!(cache.get("expired").is_none());
        assert!(cache.is_empty());
        assert!(cache.insert("replacement".to_owned(), response(Duration::from_secs(1))));
    }

    #[test]
    fn registry_isolates_routes_and_replaces_changed_capacities() {
        let registry = CacheRegistry::default();
        let first = registry.route("first", 1);
        let second = registry.route("second", 1);
        assert!(first.insert("key".to_owned(), response(Duration::from_secs(10))));
        assert!(second.get("key").is_none());

        let resized = registry.route("first", 2);
        assert!(resized.get("key").is_none());
        assert!(!Arc::ptr_eq(&first, &resized));
        assert!(Arc::ptr_eq(&resized, &registry.route("first", 2)));
    }
}
