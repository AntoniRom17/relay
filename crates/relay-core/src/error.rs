use http::StatusCode;
use thiserror::Error;

/// Errors that may be safely mapped to gateway responses.
#[derive(Debug, Error)]
pub enum GatewayError {
    /// No configured route accepted the request.
    #[error("no route matched the request")]
    RouteNotFound,
    /// Every endpoint in a selected upstream pool is unavailable.
    #[error("no healthy upstream endpoint is available")]
    NoHealthyUpstream,
    /// The selected upstream did not respond within its timeout budget.
    #[error("upstream request timed out")]
    UpstreamTimeout,
    /// The selected upstream could not complete the request.
    #[error("upstream transport failed")]
    UpstreamTransport,
    /// A protection policy deliberately rejected the request.
    #[error("request rejected by overload protection")]
    Overloaded,
    /// The upstream circuit breaker is open.
    #[error("upstream circuit breaker is open")]
    CircuitOpen,
    /// A required external policy dependency is unavailable.
    #[error("required gateway dependency is unavailable")]
    DependencyUnavailable,
    /// Authentication credentials were missing or invalid.
    #[error("authentication failed")]
    Unauthorized,
    /// A rate limit was exceeded.
    #[error("rate limit exceeded")]
    RateLimited,
    /// A request violates configured input limits.
    #[error("request is too large")]
    RequestTooLarge,
    /// An internal invariant failed without a safe client-facing explanation.
    #[error("internal gateway error")]
    Internal,
}

impl GatewayError {
    /// Returns the HTTP status code exposed to a client.
    pub const fn status_code(&self) -> StatusCode {
        match self {
            Self::RouteNotFound => StatusCode::NOT_FOUND,
            Self::NoHealthyUpstream | Self::UpstreamTransport => StatusCode::BAD_GATEWAY,
            Self::UpstreamTimeout => StatusCode::GATEWAY_TIMEOUT,
            Self::Overloaded | Self::CircuitOpen | Self::DependencyUnavailable => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::RequestTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Returns a stable machine-readable error code.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::RouteNotFound => "route_not_found",
            Self::NoHealthyUpstream => "no_healthy_upstream",
            Self::UpstreamTimeout => "upstream_timeout",
            Self::UpstreamTransport => "upstream_transport",
            Self::Overloaded => "overloaded",
            Self::CircuitOpen => "circuit_open",
            Self::DependencyUnavailable => "dependency_unavailable",
            Self::Unauthorized => "unauthorized",
            Self::RateLimited => "rate_limited",
            Self::RequestTooLarge => "request_too_large",
            Self::Internal => "internal",
        }
    }
}

/// Result type used by the data plane.
pub type Result<T> = std::result::Result<T, GatewayError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_map_to_non_success_statuses() {
        let errors = [
            GatewayError::RouteNotFound,
            GatewayError::NoHealthyUpstream,
            GatewayError::UpstreamTimeout,
            GatewayError::UpstreamTransport,
            GatewayError::Overloaded,
            GatewayError::CircuitOpen,
            GatewayError::DependencyUnavailable,
            GatewayError::Unauthorized,
            GatewayError::RateLimited,
            GatewayError::RequestTooLarge,
            GatewayError::Internal,
        ];

        assert!(errors.iter().all(|error| !error.status_code().is_success()));
        assert!(errors.iter().all(|error| !error.code().is_empty()));
    }
}
