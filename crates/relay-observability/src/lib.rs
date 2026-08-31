//! Structured logging, metrics, and distributed tracing support.

mod logging;

pub use logging::{LoggingError, init_logging};
