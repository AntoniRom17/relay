use std::{collections::BTreeMap, sync::Arc, time::Duration};

use arc_swap::ArcSwap;
use http::Request;
use relay_auth::{AuthBuildError, Authenticator};
use relay_config::{GatewayConfig, RouteConfig};
use relay_core::GatewayError;
use relay_routing::{RouteBuildError, RouteTable};
use relay_upstream::{CircuitBreaker, Pool};
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::service::{HttpsClient, build_upstream_client};

/// Immutable routing and policy state installed in the data plane.
#[derive(Debug)]
pub struct RuntimeState {
    config: Arc<GatewayConfig>,
    routes: RouteTable,
    pools: BTreeMap<String, Arc<Pool>>,
    upstream_clients: BTreeMap<String, HttpsClient>,
    circuits: BTreeMap<String, CircuitBreaker>,
    route_concurrency: BTreeMap<String, Arc<Semaphore>>,
    authenticator: Authenticator,
}

/// Atomically replaceable runtime state shared by all requests and control handlers.
#[derive(Debug)]
pub struct RuntimeStore {
    active: ArcSwap<RuntimeState>,
}

impl RuntimeStore {
    /// Installs the initial validated state.
    pub fn new(initial: RuntimeState) -> Self {
        Self {
            active: ArcSwap::from_pointee(initial),
        }
    }

    /// Loads a stable snapshot for one operation.
    pub fn load(&self) -> Arc<RuntimeState> {
        self.active.load_full()
    }

    /// Compiles and atomically installs a new configuration.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeBuildError`] without changing the active snapshot when routes or
    /// authentication material cannot be compiled.
    pub fn reload(&self, config: GatewayConfig) -> Result<(), RuntimeBuildError> {
        let replacement = Arc::new(RuntimeState::build(config)?);
        self.active.store(replacement);
        Ok(())
    }
}

impl RuntimeState {
    /// Builds request-processing state from validated configuration.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeBuildError`] when route matchers or authentication material are invalid.
    pub fn build(config: GatewayConfig) -> Result<Self, RuntimeBuildError> {
        let routes = RouteTable::compile(config.routes.clone())?;
        let pools = config
            .upstreams
            .iter()
            .map(|(name, upstream)| {
                (
                    name.clone(),
                    Arc::new(Pool::from_config(name.as_str(), upstream)),
                )
            })
            .collect();
        let circuits = config
            .upstreams
            .iter()
            .map(|(name, upstream)| (name.clone(), CircuitBreaker::new(&upstream.circuit_breaker)))
            .collect();
        let upstream_clients = config
            .upstreams
            .iter()
            .map(|(name, upstream)| {
                (
                    name.clone(),
                    build_upstream_client(Duration::from_millis(upstream.connect_timeout_ms)),
                )
            })
            .collect();
        let route_concurrency = config
            .routes
            .iter()
            .map(|route| {
                (
                    route.name.clone(),
                    Arc::new(Semaphore::new(route.concurrency_limit)),
                )
            })
            .collect();
        let authenticator = Authenticator::from_config(&config.authentication)?;

        Ok(Self {
            config: Arc::new(config),
            routes,
            pools,
            upstream_clients,
            circuits,
            route_concurrency,
            authenticator,
        })
    }

    /// Matches and admits a request to its route-level concurrency budget.
    ///
    /// # Errors
    ///
    /// Returns [`GatewayError::RouteNotFound`] for unmatched requests and
    /// [`GatewayError::Overloaded`] when the matched route has no remaining capacity.
    pub fn plan<B>(&self, request: &Request<B>) -> Result<RequestPlan, GatewayError> {
        let matched = self
            .routes
            .match_request(request)
            .ok_or(GatewayError::RouteNotFound)?;
        let route_permit = self
            .route_concurrency
            .get(&matched.route.name)
            .ok_or(GatewayError::Internal)?
            .clone()
            .try_acquire_owned()
            .map_err(|_| GatewayError::Overloaded)?;
        let pool = Arc::clone(
            self.pools
                .get(&matched.route.upstream)
                .ok_or(GatewayError::Internal)?,
        );
        let circuit = self
            .circuits
            .get(&matched.route.upstream)
            .ok_or(GatewayError::Internal)?
            .clone();
        let upstream_client = self
            .upstream_clients
            .get(&matched.route.upstream)
            .ok_or(GatewayError::Internal)?
            .clone();

        Ok(RequestPlan {
            route: matched.route,
            pool,
            circuit,
            upstream_client,
            _route_permit: route_permit,
        })
    }

    /// Returns the stable name of the route that would match without admitting concurrency.
    pub fn route_name<B>(&self, request: &Request<B>) -> Option<String> {
        self.routes
            .match_request(request)
            .map(|matched| matched.route.name.clone())
    }

    /// Returns the active validated configuration.
    pub fn config(&self) -> &Arc<GatewayConfig> {
        &self.config
    }

    /// Returns a configured pool by name.
    pub fn pool(&self, name: &str) -> Option<&Arc<Pool>> {
        self.pools.get(name)
    }

    /// Iterates through configured upstream pools.
    pub fn pools(&self) -> impl ExactSizeIterator<Item = (&str, &Arc<Pool>)> {
        self.pools.iter().map(|(name, pool)| (name.as_str(), pool))
    }

    /// Returns a configured pool circuit breaker by name.
    pub fn circuit(&self, name: &str) -> Option<&CircuitBreaker> {
        self.circuits.get(name)
    }

    /// Iterates through configured pool circuit breakers.
    pub fn circuits(&self) -> impl ExactSizeIterator<Item = (&str, &CircuitBreaker)> {
        self.circuits
            .iter()
            .map(|(name, circuit)| (name.as_str(), circuit))
    }

    /// Returns compiled authentication material.
    pub fn authenticator(&self) -> &Authenticator {
        &self.authenticator
    }
}

/// Request-scoped policy state retained until the response path completes.
#[derive(Debug)]
pub struct RequestPlan {
    /// Matched route.
    pub route: Arc<RouteConfig>,
    /// Selected upstream pool.
    pub pool: Arc<Pool>,
    /// Circuit breaker associated with the pool.
    pub circuit: CircuitBreaker,
    pub(crate) upstream_client: HttpsClient,
    _route_permit: OwnedSemaphorePermit,
}

/// Failure to compile runtime request-processing state.
#[derive(Debug, Error)]
pub enum RuntimeBuildError {
    /// Route definitions could not be compiled.
    #[error(transparent)]
    Routes(#[from] RouteBuildError),
    /// Authentication keys or providers could not be compiled.
    #[error(transparent)]
    Authentication(#[from] AuthBuildError),
}

#[cfg(test)]
mod tests {
    use http::Request;
    use relay_config::parse;

    use super::*;

    fn config(route: &str, path: &str) -> GatewayConfig {
        parse(&format!(
            r"
listeners:
  - address: 127.0.0.1:8080
routes:
  - name: {route}
    path_prefix: {path}
    upstream: api
upstreams:
  api:
    endpoints:
      - url: http://127.0.0.1:9001
redis: null
"
        ))
        .expect("test configuration should be valid")
    }

    #[test]
    fn reload_is_atomic_and_old_snapshots_remain_valid() {
        let store =
            RuntimeStore::new(RuntimeState::build(config("old", "/old")).expect("initial runtime"));
        let old_snapshot = store.load();
        store
            .reload(config("new", "/new"))
            .expect("replacement runtime");
        let new_snapshot = store.load();
        let old_request = Request::get("/old/resource").body(()).expect("request");
        let new_request = Request::get("/new/resource").body(()).expect("request");

        assert_eq!(
            old_snapshot
                .plan(&old_request)
                .expect("old route")
                .route
                .name,
            "old"
        );
        assert!(old_snapshot.plan(&new_request).is_err());
        assert_eq!(
            new_snapshot
                .plan(&new_request)
                .expect("new route")
                .route
                .name,
            "new"
        );
        assert!(new_snapshot.plan(&old_request).is_err());
        assert_eq!(
            new_snapshot.route_name(&new_request).as_deref(),
            Some("new")
        );
    }
}
