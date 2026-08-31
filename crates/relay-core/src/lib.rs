//! Shared domain types and lifecycle primitives used across Relay.

mod error;
mod request;

pub use error::{GatewayError, Result};
pub use request::RequestContext;
