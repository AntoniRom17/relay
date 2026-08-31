use relay_config::ObservabilityConfig;
use thiserror::Error;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt as _, util::SubscriberInitExt as _};

/// Error returned when the process-wide tracing subscriber cannot be installed.
#[derive(Debug, Error)]
pub enum LoggingError {
    /// The configured filter expression is invalid.
    #[error("invalid observability.log_filter: {0}")]
    Filter(#[from] tracing_subscriber::filter::ParseError),
    /// The subscriber could not be installed, usually because one is already active.
    #[error("failed to install tracing subscriber: {0}")]
    Subscriber(#[from] tracing_subscriber::util::TryInitError),
}

/// Installs Relay's process-wide structured tracing subscriber.
pub fn init_logging(config: &ObservabilityConfig) -> Result<(), LoggingError> {
    let filter = EnvFilter::try_new(&config.log_filter)?;
    let registry = tracing_subscriber::registry().with(filter);

    if config.json_logs {
        registry
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .flatten_event(true)
                    .with_current_span(true)
                    .with_span_list(false),
            )
            .try_init()?;
    } else {
        registry
            .with(
                tracing_subscriber::fmt::layer()
                    .compact()
                    .with_target(true)
                    .with_thread_ids(true),
            )
            .try_init()?;
    }

    Ok(())
}
