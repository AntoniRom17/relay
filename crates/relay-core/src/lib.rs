//! Shared domain types and lifecycle primitives used across Relay.

mod error;
mod lifecycle;
mod request;

pub use error::{GatewayError, Result};
pub use lifecycle::Shutdown;
pub use request::RequestContext;
