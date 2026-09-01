//! Structured logging, metrics, and distributed tracing support.

mod logging;
mod metrics;

pub use logging::{LoggingError, TelemetryGuard, init_logging};
pub use metrics::Metrics;

use http::{HeaderMap, HeaderName, HeaderValue};
use opentelemetry::{
    Context, global,
    propagation::{Extractor, Injector},
};
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

struct HeaderExtractor<'a>(&'a HeaderMap);

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|value| value.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(HeaderName::as_str).collect()
    }
}

struct HeaderInjector<'a>(&'a mut HeaderMap);

impl Injector for HeaderInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(key.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            self.0.insert(name, value);
        }
    }
}

/// Assigns an incoming W3C trace context as the parent of a request span.
pub fn set_parent_from_headers(span: &tracing::Span, headers: &HeaderMap) {
    let parent =
        global::get_text_map_propagator(|propagator| propagator.extract(&HeaderExtractor(headers)));
    span.set_parent(parent);
}

/// Injects the current span's W3C trace context into outgoing HTTP headers.
pub fn inject_context(headers: &mut HeaderMap) {
    let context: Context = tracing::Span::current().context();
    global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&context, &mut HeaderInjector(headers));
    });
}

#[cfg(test)]
mod tests {
    use opentelemetry::{global, propagation::TextMapPropagator as _, trace::TraceContextExt as _};
    use opentelemetry_sdk::propagation::TraceContextPropagator;

    use super::*;

    #[test]
    fn http_carriers_round_trip_w3c_trace_context() {
        global::set_text_map_propagator(TraceContextPropagator::new());
        let mut incoming = HeaderMap::new();
        incoming.insert(
            "traceparent",
            HeaderValue::from_static("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
        );
        let context = global::get_text_map_propagator(|propagator| {
            propagator.extract(&HeaderExtractor(&incoming))
        });
        assert!(context.span().span_context().is_valid());

        let mut outgoing = HeaderMap::new();
        TraceContextPropagator::new().inject_context(&context, &mut HeaderInjector(&mut outgoing));

        assert_eq!(outgoing["traceparent"], incoming["traceparent"]);
    }
}
