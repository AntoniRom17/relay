use std::{collections::BTreeMap, net::SocketAddr};

use serde::{Deserialize, Serialize};
use url::Url;

/// Complete configuration atomically installed into a Relay process.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayConfig {
    /// Public data-plane listeners.
    pub listeners: Vec<ListenerConfig>,
    /// Dedicated administrative listener.
    #[serde(default)]
    pub admin: AdminConfig,
    /// Ordered routing rules.
    pub routes: Vec<RouteConfig>,
    /// Named pools of backend endpoints.
    pub upstreams: BTreeMap<String, UpstreamConfig>,
    /// Authentication material referenced by route policies.
    #[serde(default)]
    pub authentication: AuthenticationConfig,
    /// Logging, metrics, and tracing settings.
    #[serde(default)]
    pub observability: ObservabilityConfig,
    /// Optional Redis connection used for distributed policies.
    pub redis: Option<RedisConfig>,
    /// Optional gateway-wide aggregate rate limit applied before routing.
    #[serde(default)]
    pub gateway_rate_limit: Option<RateLimitConfig>,
    /// Maximum time allowed for in-flight work during shutdown.
    #[serde(default = "default_shutdown_grace_ms")]
    pub shutdown_grace_ms: u64,
}

/// A public socket accepting proxied traffic.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ListenerConfig {
    /// Socket address to bind.
    pub address: SocketAddr,
    /// Optional TLS certificate and key.
    pub tls: Option<TlsConfig>,
    /// Maximum concurrently accepted connections.
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
    /// Maximum duration of a complete client request.
    #[serde(default = "default_request_timeout_ms")]
    pub request_timeout_ms: u64,
    /// Maximum accepted request header bytes.
    #[serde(default = "default_max_header_bytes")]
    pub max_header_bytes: usize,
}

/// TLS identity loaded by a public listener.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    /// PEM certificate chain path.
    pub certificate: String,
    /// PEM private key path.
    pub private_key: String,
}

/// Administrative API configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AdminConfig {
    /// Whether to start the control plane.
    pub enabled: bool,
    /// Loopback address used by default to avoid accidental exposure.
    pub address: SocketAddr,
    /// Explicitly allow a non-loopback bind for network-isolated container deployments.
    pub allow_remote: bool,
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            address: "127.0.0.1:9090"
                .parse()
                .expect("static socket address is valid"),
            allow_remote: false,
        }
    }
}

/// A routing rule and the policies applied after it matches.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RouteConfig {
    /// Stable unique name used in logs and metrics.
    pub name: String,
    /// Optional case-insensitive host matcher, without a port.
    pub host: Option<String>,
    /// Exact request path matcher.
    pub path_exact: Option<String>,
    /// Request path prefix matcher.
    pub path_prefix: Option<String>,
    /// Allowed methods. An empty set accepts every method.
    #[serde(default)]
    pub methods: Vec<String>,
    /// Required exact request header values.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Upstream pool selected by this route.
    pub upstream: String,
    /// Per-route retry behavior.
    #[serde(default)]
    pub retry: RetryConfig,
    /// Optional local or distributed rate limit.
    pub rate_limit: Option<RateLimitConfig>,
    /// Optional authentication policy.
    pub authentication: Option<AuthPolicyConfig>,
    /// Optional response cache policy.
    pub cache: Option<CacheConfig>,
    /// Maximum request body size for this route.
    #[serde(default = "default_body_limit")]
    pub max_body_bytes: u64,
    /// Maximum in-flight requests admitted to this route.
    #[serde(default = "default_route_concurrency")]
    pub concurrency_limit: usize,
}

/// Retry settings applied only when the request is safe to replay.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RetryConfig {
    /// Total forwarding attempts, including the first attempt.
    pub max_attempts: u32,
    /// Initial exponential backoff duration.
    pub base_backoff_ms: u64,
    /// Maximum exponential backoff duration.
    pub max_backoff_ms: u64,
    /// HTTP statuses eligible for a retry.
    pub statuses: Vec<u16>,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 1,
            base_backoff_ms: 25,
            max_backoff_ms: 500,
            statuses: vec![502, 503, 504],
        }
    }
}

/// Per-key token bucket policy.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RateLimitConfig {
    /// Refill rate in tokens per second.
    pub requests_per_second: u32,
    /// Maximum accumulated tokens.
    pub burst_capacity: u32,
    /// Identity used to select an independent bucket.
    #[serde(default)]
    pub key: RateLimitKey,
    /// Use Redis to coordinate limits between Relay replicas.
    #[serde(default)]
    pub distributed: bool,
}

/// Dimension by which rate-limit state is partitioned.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitKey {
    /// One bucket for the route.
    #[default]
    Route,
    /// A bucket for each connecting IP address.
    ClientIp,
    /// A bucket for each presented API key.
    ApiKey,
}

/// Authentication required by a route.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthPolicyConfig {
    /// Match an API key from a configured header.
    ApiKey {
        /// Header carrying the credential.
        #[serde(default = "default_api_key_header")]
        header: String,
    },
    /// Validate a bearer JWT against configured issuer material.
    Jwt {
        /// Name of a configured JWT issuer.
        provider: String,
    },
}

/// Cache behavior for eligible GET and HEAD responses.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CacheConfig {
    /// Maximum age of a cached response.
    pub ttl_seconds: u64,
    /// Maximum entries retained by the bounded in-memory cache.
    #[serde(default = "default_cache_entries")]
    pub max_entries: u64,
    /// Largest response body retained while still streaming it downstream.
    #[serde(default = "default_cache_body_bytes")]
    pub max_body_bytes: usize,
}

/// Authentication secrets and trust material.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthenticationConfig {
    /// SHA-256 API key digests encoded as hexadecimal strings.
    pub api_key_sha256: Vec<String>,
    /// Named JWT providers.
    pub jwt_providers: BTreeMap<String, JwtProviderConfig>,
}

/// A JWT verification policy.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JwtProviderConfig {
    /// Expected issuer claim.
    pub issuer: String,
    /// Expected audience claim.
    pub audience: String,
    /// PEM public key used for signature verification.
    pub public_key: String,
    /// Allowed asymmetric algorithm.
    #[serde(default)]
    pub algorithm: JwtAlgorithm,
}

/// Supported JWT signature algorithms.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
pub enum JwtAlgorithm {
    /// RSA with SHA-256.
    #[default]
    RS256,
    /// ECDSA P-256 with SHA-256.
    ES256,
    /// Ed25519.
    EdDSA,
}

/// A named upstream pool.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpstreamConfig {
    /// Backend endpoints in this pool.
    pub endpoints: Vec<EndpointConfig>,
    /// Endpoint selection strategy.
    #[serde(default)]
    pub load_balancing: LoadBalancingPolicy,
    /// Active health-check policy.
    pub health_check: Option<HealthCheckConfig>,
    /// Circuit-breaker policy.
    #[serde(default)]
    pub circuit_breaker: CircuitBreakerConfig,
    /// Maximum duration allowed to establish a connection.
    #[serde(default = "default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,
    /// Maximum in-flight requests across this pool.
    #[serde(default = "default_upstream_concurrency")]
    pub concurrency_limit: usize,
}

/// A backend URL and scheduling weight.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointConfig {
    /// HTTP or HTTPS origin URL.
    pub url: Url,
    /// Positive relative weight for weighted round-robin.
    #[serde(default = "default_endpoint_weight")]
    pub weight: u32,
}

/// Upstream endpoint scheduling algorithm.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LoadBalancingPolicy {
    /// Equal rotation through healthy endpoints.
    #[default]
    RoundRobin,
    /// Rotation proportional to configured endpoint weight.
    WeightedRoundRobin,
    /// Select the healthy endpoint with the fewest in-flight requests.
    LeastConnections,
}

/// Active upstream health-check policy.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HealthCheckConfig {
    /// Path requested from every endpoint.
    #[serde(default = "default_health_path")]
    pub path: String,
    /// Time between check rounds.
    #[serde(default = "default_health_interval_ms")]
    pub interval_ms: u64,
    /// Per-check timeout.
    #[serde(default = "default_health_timeout_ms")]
    pub timeout_ms: u64,
    /// Inclusive minimum accepted status.
    #[serde(default = "default_healthy_status_min")]
    pub expected_status_min: u16,
    /// Inclusive maximum accepted status.
    #[serde(default = "default_healthy_status_max")]
    pub expected_status_max: u16,
    /// Consecutive successes required to recover.
    #[serde(default = "default_health_threshold")]
    pub healthy_threshold: u32,
    /// Consecutive failures required to eject.
    #[serde(default = "default_health_threshold")]
    pub unhealthy_threshold: u32,
}

/// Per-pool circuit-breaker policy.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CircuitBreakerConfig {
    /// Consecutive failures before the circuit opens.
    pub failure_threshold: u32,
    /// Time in the open state before probes are admitted.
    pub recovery_timeout_ms: u64,
    /// Maximum concurrent probes while half-open.
    pub half_open_probe_limit: u32,
    /// Successful probes required to close the circuit.
    pub success_threshold: u32,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            recovery_timeout_ms: 30_000,
            half_open_probe_limit: 1,
            success_threshold: 2,
        }
    }
}

/// Redis connectivity for optional distributed features.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RedisConfig {
    /// Redis connection URL. Environment interpolation is performed by deployment tooling.
    pub url: String,
    /// Namespace preventing collisions with other applications.
    #[serde(default = "default_redis_prefix")]
    pub key_prefix: String,
}

/// Logs, metrics, and distributed tracing settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ObservabilityConfig {
    /// Tracing filter in `tracing-subscriber` syntax.
    pub log_filter: String,
    /// Emit structured JSON logs instead of human-readable text.
    pub json_logs: bool,
    /// Optional OTLP collector endpoint.
    pub otlp_endpoint: Option<String>,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            log_filter: "relay=info".to_owned(),
            json_logs: true,
            otlp_endpoint: None,
        }
    }
}

const fn default_shutdown_grace_ms() -> u64 {
    30_000
}
const fn default_max_connections() -> usize {
    10_000
}
const fn default_request_timeout_ms() -> u64 {
    30_000
}
const fn default_max_header_bytes() -> usize {
    32 * 1024
}
const fn default_body_limit() -> u64 {
    16 * 1024 * 1024
}
const fn default_route_concurrency() -> usize {
    1_024
}
const fn default_cache_entries() -> u64 {
    10_000
}
const fn default_cache_body_bytes() -> usize {
    1024 * 1024
}
const fn default_connect_timeout_ms() -> u64 {
    3_000
}
const fn default_upstream_concurrency() -> usize {
    2_048
}
const fn default_endpoint_weight() -> u32 {
    1
}
fn default_health_path() -> String {
    "/health".to_owned()
}
const fn default_health_interval_ms() -> u64 {
    5_000
}
const fn default_health_timeout_ms() -> u64 {
    1_000
}
const fn default_healthy_status_min() -> u16 {
    200
}
const fn default_healthy_status_max() -> u16 {
    399
}
const fn default_health_threshold() -> u32 {
    2
}
fn default_api_key_header() -> String {
    "x-api-key".to_owned()
}
fn default_redis_prefix() -> String {
    "relay".to_owned()
}
