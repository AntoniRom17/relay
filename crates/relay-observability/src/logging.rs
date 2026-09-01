use opentelemetry::{KeyValue, global, trace::TracerProvider as _};
use opentelemetry_otlp::WithExportConfig as _;
use opentelemetry_sdk::{Resource, propagation::TraceContextPropagator, trace::SdkTracerProvider};
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
    /// The configured OTLP exporter could not be constructed.
    #[error("failed to initialize OTLP trace exporter: {0}")]
    Exporter(#[from] opentelemetry_otlp::ExporterBuildError),
}

/// Keeps the SDK tracer provider alive and flushes queued spans on shutdown.
#[derive(Debug, Default)]
pub struct TelemetryGuard {
    provider: Option<SdkTracerProvider>,
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(provider) = self.provider.take() {
            let _ = provider.shutdown();
        }
    }
}

/// Installs Relay's process-wide structured tracing subscriber.
pub fn init_logging(config: &ObservabilityConfig) -> Result<TelemetryGuard, LoggingError> {
    let filter = EnvFilter::try_new(&config.log_filter)?;
    global::set_text_map_propagator(TraceContextPropagator::new());
    let provider = config
        .otlp_endpoint
        .as_deref()
        .map(build_provider)
        .transpose()?;
    let telemetry = provider
        .as_ref()
        .map(|provider| tracing_opentelemetry::layer().with_tracer(provider.tracer("relay")));
    let registry = tracing_subscriber::registry().with(filter).with(telemetry);

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

    Ok(TelemetryGuard { provider })
}

fn build_provider(
    endpoint: &str,
) -> Result<SdkTracerProvider, opentelemetry_otlp::ExporterBuildError> {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()?;
    let resource = Resource::builder()
        .with_attributes([KeyValue::new("service.name", "relay")])
        .build();
    Ok(SdkTracerProvider::builder()
        .with_resource(resource)
        .with_batch_exporter(exporter)
        .build())
}
