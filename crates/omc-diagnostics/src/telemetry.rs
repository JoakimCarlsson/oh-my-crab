//! OpenTelemetry export: the spans and metrics of the `tracing` macros, sent to a collector over OTLP.
//!
//! Spans become traces. Events with fields named `histogram.<name>`,
//! `counter.<name>` or `monotonic_counter.<name>` become metrics of that name,
//! with the event's other fields as attributes.

/// The variables that name where the standard OTLP exporter sends; one of them must be set for anything to be sent.
const ENDPOINT_VARIABLES: [&str; 3] = [
    "OTEL_EXPORTER_OTLP_ENDPOINT",
    "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
    "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
];

/// Telemetry being sent to an OpenTelemetry collector; [`Telemetry::shutdown`] sends what is still held.
pub struct Telemetry {
    /// Where finished spans go.
    #[cfg(feature = "otlp")]
    traces: opentelemetry_sdk::trace::SdkTracerProvider,
    /// Where metrics go.
    #[cfg(feature = "otlp")]
    metrics: opentelemetry_sdk::metrics::SdkMeterProvider,
}

impl Telemetry {
    /// Starts sending this process's spans and metrics as the service `service` at `version`, over OTLP with HTTP and protobuf, as the standard `OTEL_*` variables configure it.
    ///
    /// Gives nothing, and sends nothing, when no endpoint variable is set, when `OTEL_SDK_DISABLED` is `true`, when the crate was built without the `otlp` feature, or when the exporter cannot be built or another subscriber is set already; the last two are reported on standard error.
    pub fn start(service: &'static str, version: &'static str) -> Option<Self> {
        let wanted = ENDPOINT_VARIABLES
            .iter()
            .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()));
        let disabled = std::env::var("OTEL_SDK_DISABLED")
            .is_ok_and(|value| value.trim().eq_ignore_ascii_case("true"));
        if !wanted || disabled {
            return None;
        }
        Self::export(service, version)
    }

    /// Builds the exporters and sets the subscriber that feeds them, reporting a failure on standard error.
    #[cfg(feature = "otlp")]
    fn export(service: &'static str, version: &'static str) -> Option<Self> {
        match otlp::start(service, version) {
            Ok(telemetry) => Some(telemetry),
            Err(reason) => {
                eprintln!("{service}: telemetry is not sent: {reason}");
                None
            }
        }
    }

    /// Says on standard error that telemetry was asked for in a build that cannot send it.
    #[cfg(not(feature = "otlp"))]
    fn export(service: &'static str, _version: &'static str) -> Option<Self> {
        eprintln!(
            "{service}: telemetry is not sent: this build has no OpenTelemetry export (the otlp feature)"
        );
        None
    }

    /// Sends the spans and metrics still held and stops sending; a failure is reported on standard error.
    pub fn shutdown(self) {
        #[cfg(feature = "otlp")]
        {
            if let Err(error) = self.traces.shutdown() {
                eprintln!("telemetry: traces were not all sent: {error}");
            }
            if let Err(error) = self.metrics.shutdown() {
                eprintln!("telemetry: metrics were not all sent: {error}");
            }
        }
    }
}

#[cfg(feature = "otlp")]
/// The OTLP exporters and the `tracing` layers that feed them.
mod otlp {
    use opentelemetry::trace::TracerProvider;
    use opentelemetry_otlp::{MetricExporter, SpanExporter, WithExportConfig};
    use opentelemetry_sdk::Resource;
    use opentelemetry_sdk::metrics::SdkMeterProvider;
    use opentelemetry_sdk::trace::SdkTracerProvider;
    use tracing_opentelemetry::{MetricsLayer, OpenTelemetryLayer};
    use tracing_subscriber::layer::SubscriberExt;

    use super::Telemetry;

    /// Builds the trace and metric exporters for `service` at `version` and sets the global `tracing` subscriber that feeds them.
    ///
    /// # Errors
    ///
    /// Fails when an exporter cannot be built or a global subscriber is set already.
    pub(super) fn start(service: &'static str, version: &'static str) -> Result<Telemetry, String> {
        let resource = Resource::builder()
            .with_service_name(service)
            .with_attribute(opentelemetry::KeyValue::new("service.version", version))
            .build();
        let spans = SpanExporter::builder()
            .with_http()
            .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
            .build()
            .map_err(|error| error.to_string())?;
        let measures = MetricExporter::builder()
            .with_http()
            .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
            .build()
            .map_err(|error| error.to_string())?;
        let traces = SdkTracerProvider::builder()
            .with_resource(resource.clone())
            .with_batch_exporter(spans)
            .build();
        let metrics = SdkMeterProvider::builder()
            .with_resource(resource)
            .with_periodic_exporter(measures)
            .build();
        let subscriber = tracing_subscriber::registry()
            .with(OpenTelemetryLayer::new(traces.tracer(service)))
            .with(MetricsLayer::new(metrics.clone()));
        tracing::subscriber::set_global_default(subscriber).map_err(|error| error.to_string())?;
        Ok(Telemetry { traces, metrics })
    }
}
