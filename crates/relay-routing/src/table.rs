use std::{cmp::Reverse, str::FromStr as _, sync::Arc};

use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, Uri, uri::Authority};
use relay_config::RouteConfig;
use thiserror::Error;

/// A route that matched an incoming request.
#[derive(Debug, Clone)]
pub struct RouteMatch {
    /// The immutable route configuration used by downstream policy layers.
    pub route: Arc<RouteConfig>,
}

/// Failure while compiling an otherwise typed route definition.
#[derive(Debug, Error)]
pub enum RouteBuildError {
    /// A method token was invalid.
    #[error("route `{route}` contains invalid HTTP method `{method}`")]
    Method {
        /// Route name.
        route: String,
        /// Invalid method token.
        method: String,
    },
    /// A required header name was invalid.
    #[error("route `{route}` contains invalid header name `{name}`")]
    HeaderName {
        /// Route name.
        route: String,
        /// Invalid name.
        name: String,
    },
    /// A required header value was invalid.
    #[error("route `{route}` contains invalid value for header `{name}`")]
    HeaderValue {
        /// Route name.
        route: String,
        /// Header name associated with the invalid value.
        name: String,
    },
}

/// An immutable, precedence-sorted routing table.
#[derive(Debug, Clone, Default)]
pub struct RouteTable {
    routes: Arc<[CompiledRoute]>,
}

#[derive(Debug, Clone)]
struct CompiledRoute {
    config: Arc<RouteConfig>,
    host: Option<Box<str>>,
    path: PathMatcher,
    methods: Box<[Method]>,
    headers: Box<[(HeaderName, HeaderValue)]>,
    declaration_index: usize,
}

#[derive(Debug, Clone)]
enum PathMatcher {
    Exact(Box<str>),
    Prefix(Box<str>),
    Any,
}

impl RouteTable {
    /// Compiles route strings into HTTP types and sorts them by specificity.
    ///
    /// # Errors
    ///
    /// Returns [`RouteBuildError`] when a method or required header cannot be compiled.
    pub fn compile(routes: Vec<RouteConfig>) -> Result<Self, RouteBuildError> {
        let mut compiled = routes
            .into_iter()
            .enumerate()
            .map(|(declaration_index, route)| CompiledRoute::compile(route, declaration_index))
            .collect::<Result<Vec<_>, _>>()?;

        compiled.sort_by_key(|route| Reverse(route.precedence()));
        Ok(Self {
            routes: compiled.into(),
        })
    }

    /// Selects the highest-precedence route matching the request.
    pub fn match_request<B>(&self, request: &Request<B>) -> Option<RouteMatch> {
        let host = request_host(request.uri(), request.headers());
        self.routes
            .iter()
            .find(|route| {
                route.matches(
                    request.method(),
                    request.uri().path(),
                    host,
                    request.headers(),
                )
            })
            .map(|route| RouteMatch {
                route: Arc::clone(&route.config),
            })
    }

    /// Returns routes in effective precedence order.
    pub fn routes(&self) -> impl ExactSizeIterator<Item = &Arc<RouteConfig>> {
        self.routes.iter().map(|route| &route.config)
    }
}

impl CompiledRoute {
    fn compile(config: RouteConfig, declaration_index: usize) -> Result<Self, RouteBuildError> {
        let host = config
            .host
            .as_deref()
            .map(str::to_ascii_lowercase)
            .map(Into::into);
        let path = if let Some(exact) = &config.path_exact {
            PathMatcher::Exact(exact.clone().into_boxed_str())
        } else if let Some(prefix) = &config.path_prefix {
            PathMatcher::Prefix(prefix.clone().into_boxed_str())
        } else {
            PathMatcher::Any
        };
        let methods = config
            .methods
            .iter()
            .map(|method| {
                Method::from_bytes(method.as_bytes()).map_err(|_| RouteBuildError::Method {
                    route: config.name.clone(),
                    method: method.clone(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice();
        let headers = config
            .headers
            .iter()
            .map(|(name, value)| {
                let name = HeaderName::from_str(name).map_err(|_| RouteBuildError::HeaderName {
                    route: config.name.clone(),
                    name: name.clone(),
                })?;
                let value =
                    HeaderValue::from_str(value).map_err(|_| RouteBuildError::HeaderValue {
                        route: config.name.clone(),
                        name: name.to_string(),
                    })?;
                Ok((name, value))
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice();

        Ok(Self {
            config: Arc::new(config),
            host,
            path,
            methods,
            headers,
            declaration_index,
        })
    }

    fn precedence(&self) -> (u8, u8, usize, usize, usize, Reverse<usize>) {
        let (path_kind, path_length) = match &self.path {
            PathMatcher::Exact(path) => (2, path.len()),
            PathMatcher::Prefix(path) => (1, path.len()),
            PathMatcher::Any => (0, 0),
        };
        (
            u8::from(self.host.is_some()),
            path_kind,
            path_length,
            self.methods.len(),
            self.headers.len(),
            Reverse(self.declaration_index),
        )
    }

    fn matches(
        &self,
        method: &Method,
        path: &str,
        host: Option<&str>,
        headers: &HeaderMap,
    ) -> bool {
        if let Some(expected) = self.host.as_deref()
            && !host.is_some_and(|actual| actual.eq_ignore_ascii_case(expected))
        {
            return false;
        }
        match &self.path {
            PathMatcher::Exact(expected) if path != expected.as_ref() => return false,
            PathMatcher::Prefix(expected) if !path.starts_with(expected.as_ref()) => return false,
            PathMatcher::Exact(_) | PathMatcher::Prefix(_) | PathMatcher::Any => {}
        }
        if !self.methods.is_empty() && !self.methods.contains(method) {
            return false;
        }
        self.headers
            .iter()
            .all(|(name, value)| headers.get(name).is_some_and(|actual| actual == value))
    }
}

fn request_host<'a>(uri: &'a Uri, headers: &'a HeaderMap) -> Option<&'a str> {
    uri.authority().map(Authority::host).or_else(|| {
        headers
            .get(http::header::HOST)
            .and_then(|value| value.to_str().ok())
            .and_then(strip_host_port)
    })
}

fn strip_host_port(authority: &str) -> Option<&str> {
    if authority.starts_with('[') {
        let end = authority.find(']')?;
        Some(&authority[1..end])
    } else {
        Some(
            authority
                .split_once(':')
                .map_or(authority, |(host, _)| host),
        )
    }
}
