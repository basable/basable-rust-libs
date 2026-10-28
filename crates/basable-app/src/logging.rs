//! Tracing to stdout as JSON lines, filtered by `RUST_LOG` (default
//! `info`). Exposed as `basable_app::tracing`.
//!
//! When the platform has injected an OTLP endpoint
//! (`OTEL_EXPORTER_OTLP_ENDPOINT`: every container of a project gets the node
//! agent's, `http://<node>:4318`), [`init_json`] also starts the
//! OpenTelemetry SDK: every `tracing` span becomes an OpenTelemetry span, and
//! a global meter provider takes the app's own metrics
//! (`opentelemetry::global::meter`), both exported over OTLP HTTP protobuf.
//! `service.name` is `OTEL_SERVICE_NAME`, else the binary's name. The run's
//! drain flushes them ([`shutdown`]). Without the variable nothing changes:
//! logs go to stdout, which the platform collects either way.

use std::sync::OnceLock;

use basable_core::BoxError;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::{SdkTracerProvider, Tracer};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// The SDK this process started, for [`shutdown`].
static OTLP: OnceLock<Otlp> = OnceLock::new();

/// Installs the JSON subscriber, and the OpenTelemetry SDK when an OTLP
/// endpoint is set. Idempotent: a second call (a test that boots two apps)
/// is a no-op.
pub fn init_json() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let tracer = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .ok()
        .filter(|endpoint| !endpoint.trim().is_empty())
        .and_then(|endpoint| {
            if let Some(started) = OTLP.get() {
                return Some(started.tracer());
            }
            match Otlp::start(&endpoint, &service_name()) {
                Ok(otlp) => {
                    otlp.install_globals();
                    let tracer = otlp.tracer();
                    let _ = OTLP.set(otlp);
                    Some(tracer)
                }
                Err(err) => {
                    // Nothing is listening for tracing yet: stderr is where
                    // this reaches the platform's logs.
                    eprintln!("basable-app: OpenTelemetry not started ({endpoint}): {err}");
                    None
                }
            }
        });
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_current_span(false),
        )
        .with(tracer.map(|tracer| tracing_opentelemetry::layer().with_tracer(tracer)))
        .try_init();
}

/// Installs a human-readable subscriber, for local runs and tests.
pub fn init_pretty() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

/// Flushes and stops the OpenTelemetry SDK, if [`init_json`] started it:
/// the run's drain calls it last, so the final spans and metrics are sent.
pub fn shutdown() {
    if let Some(otlp) = OTLP.get() {
        otlp.shutdown();
    }
}

/// The service's name: `OTEL_SERVICE_NAME`, else the binary's.
fn service_name() -> String {
    if let Ok(name) = std::env::var("OTEL_SERVICE_NAME")
        && !name.trim().is_empty()
    {
        return name;
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "app".to_string())
}

/// The OpenTelemetry providers: spans and metrics to one OTLP endpoint.
struct Otlp {
    tracer: SdkTracerProvider,
    meter: SdkMeterProvider,
}

impl Otlp {
    /// Builds both providers for `endpoint` (`http://host:4318`, the paths
    /// `/v1/traces` and `/v1/metrics` appended). Their exports run on the
    /// SDK's own threads, not the async runtime's.
    fn start(endpoint: &str, service: &str) -> Result<Otlp, BoxError> {
        let base = endpoint.trim().trim_end_matches('/');
        let resource = Resource::builder()
            .with_service_name(service.to_string())
            .build();
        let spans = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(format!("{base}/v1/traces"))
            .build()?;
        let metrics = opentelemetry_otlp::MetricExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(format!("{base}/v1/metrics"))
            .build()?;
        Ok(Otlp {
            tracer: SdkTracerProvider::builder()
                .with_batch_exporter(spans)
                .with_resource(resource.clone())
                .build(),
            meter: SdkMeterProvider::builder()
                .with_periodic_exporter(metrics)
                .with_resource(resource)
                .build(),
        })
    }

    /// Makes these the process's providers and W3C trace context its
    /// propagator.
    fn install_globals(&self) {
        opentelemetry::global::set_tracer_provider(self.tracer.clone());
        opentelemetry::global::set_meter_provider(self.meter.clone());
        opentelemetry::global::set_text_map_propagator(
            opentelemetry_sdk::propagation::TraceContextPropagator::new(),
        );
    }

    fn tracer(&self) -> Tracer {
        self.tracer.tracer("basable-app")
    }

    /// Flushes what is buffered and stops; errors are the export's, and the
    /// process is ending either way.
    fn shutdown(&self) {
        let _ = self.tracer.shutdown();
        let _ = self.meter.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use opentelemetry::metrics::MeterProvider as _;
    use tracing_subscriber::layer::SubscriberExt;

    use super::Otlp;

    /// A plain HTTP listener that answers 200 and records each request's
    /// method, path and content type.
    fn collector() -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                let mut request = String::new();
                if reader.read_line(&mut request).is_err() {
                    continue;
                }
                let (mut length, mut content_type) = (0usize, String::new());
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
                        break;
                    }
                    let lower = header.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap_or(0);
                    }
                    if let Some(v) = lower.strip_prefix("content-type:") {
                        content_type = v.trim().to_string();
                    }
                }
                let mut body = vec![0u8; length];
                let _ = reader.read_exact(&mut body);
                let line: Vec<&str> = request.split_whitespace().take(2).collect();
                record
                    .lock()
                    .expect("lock")
                    .push(format!("{} {}", line.join(" "), content_type));
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                );
            }
        });
        (format!("http://{addr}"), seen)
    }

    #[test]
    fn spans_and_metrics_are_sent_to_the_endpoint_as_otlp() {
        let (endpoint, seen) = collector();
        let otlp = Otlp::start(&endpoint, "orders").expect("start");

        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(otlp.tracer()));
        tracing::subscriber::with_default(subscriber, || {
            let _span = tracing::info_span!("handle_order").entered();
        });
        otlp.meter
            .meter("orders")
            .u64_counter("orders_placed")
            .build()
            .add(1, &[]);
        otlp.shutdown();

        let seen = seen.lock().expect("lock").clone();
        assert!(
            seen.contains(&"POST /v1/traces application/x-protobuf".to_string()),
            "{seen:?}"
        );
        assert!(
            seen.contains(&"POST /v1/metrics application/x-protobuf".to_string()),
            "{seen:?}"
        );
    }
}
