//! Telemetry **initialization** (the `otel` feature).
//!
//! Installs the global pipeline once at a binary's entry point:
//! - a `tracing-subscriber` `fmt` layer (text or JSON) to stderr — always on;
//! - when [`OtelConfig`] is present, OTLP **trace**, **log**, and **metric**
//!   exporters, an optional Prometheus `/metrics` pull endpoint, and the W3C
//!   `traceparent` propagator.
//!
//! Init never aborts a process: a failed exporter build degrades to a warning
//! and a reduced pipeline; an already-installed subscriber degrades to a no-op.
//! Emitting telemetry (the `tracing` macros and [`crate::metrics`]) stays no-op
//! until this runs.

use std::net::SocketAddr;

use opentelemetry::KeyValue;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
use tracing_subscriber::Layer;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, Registry, fmt};

/// The OTel instrumentation scope and default `service.name`.
const SCOPE: &str = "pawrly";

/// Log line format for the `fmt` layer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LogFormat {
    /// Human-readable, single-line-per-event text (today's behaviour).
    #[default]
    Text,
    /// Line-delimited JSON, ready for any log pipeline.
    Json,
}

/// OTLP transport for the exporters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OtelProtocol {
    /// OTLP over gRPC (default endpoint `:4317`).
    #[default]
    Grpc,
    /// OTLP over HTTP/protobuf (default endpoint `:4318`).
    Http,
}

/// Which binary/role is initializing telemetry. Seeds the `service.role`
/// resource attribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceRole {
    /// In-process CLI invocation.
    Cli,
    /// The `serve` daemon (gRPC).
    Daemon,
    /// MCP over stdio.
    McpStdio,
    /// MCP over HTTP.
    McpHttp,
}

impl ServiceRole {
    /// Stable lowercase identifier, used as a log field and the OTel
    /// `service.role` resource attribute.
    pub fn as_str(self) -> &'static str {
        match self {
            ServiceRole::Cli => "cli",
            ServiceRole::Daemon => "daemon",
            ServiceRole::McpStdio => "mcp_stdio",
            ServiceRole::McpHttp => "mcp_http",
        }
    }
}

/// Prometheus pull-endpoint settings.
#[derive(Clone, Copy, Debug)]
pub struct PrometheusConfig {
    /// Address the `/metrics` endpoint listens on.
    pub listen: SocketAddr,
}

/// OTLP export settings. Present (`Some`) only when the operator has opted into
/// export; absent keeps a fmt-only pipeline with no exporters.
#[derive(Clone, Debug)]
pub struct OtelConfig {
    /// Collector endpoint (e.g. `http://localhost:4317`).
    pub endpoint: String,
    /// gRPC or HTTP transport.
    pub protocol: OtelProtocol,
    /// `service.name` resource attribute.
    pub service_name: String,
    /// Export distributed traces.
    pub traces: bool,
    /// Bridge `tracing` events to OTel log records and export them.
    pub logs: bool,
    /// Export metrics over OTLP push.
    pub metrics: bool,
    /// Parent-based ratio sampler probability in `[0.0, 1.0]`.
    pub sample_ratio: f64,
    /// Serve a Prometheus pull endpoint, independent of OTLP push.
    pub prometheus: Option<PrometheusConfig>,
}

/// Inputs for [`init`]. The CLI populates this from its flags; it can also be
/// built from `pawrly_config`'s observability block.
#[derive(Clone, Debug)]
pub struct TelemetryConfig {
    /// `EnvFilter` directive (e.g. `info`, `pawrly=debug`). `RUST_LOG` still
    /// wins when set, matching prior CLI behaviour.
    pub level: String,
    /// Text or JSON log lines.
    pub format: LogFormat,
    /// OTLP export, or `None` for fmt-only.
    pub otel: Option<OtelConfig>,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            format: LogFormat::Text,
            otel: None,
        }
    }
}

/// Guard returned by [`init`]. Holding it keeps the pipeline alive; dropping it
/// flushes and shuts down the OTel exporters (so buffered spans/logs/metrics are
/// not lost on exit) and stops the Prometheus server. Bind it for the process
/// lifetime.
#[must_use = "hold the guard for the process lifetime; dropping it flushes the OTel exporters"]
#[derive(Debug, Default)]
pub struct TelemetryGuard {
    tracer_provider: Option<SdkTracerProvider>,
    logger_provider: Option<SdkLoggerProvider>,
    meter_provider: Option<SdkMeterProvider>,
    prometheus_task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        // Flush on the way out. Errors here are unactionable (we are exiting)
        // and the subscriber may already be torn down, so they are swallowed.
        if let Some(tp) = &self.tracer_provider {
            let _ = tp.shutdown();
        }
        if let Some(lp) = &self.logger_provider {
            let _ = lp.shutdown();
        }
        if let Some(mp) = &self.meter_provider {
            let _ = mp.shutdown();
        }
        if let Some(task) = &self.prometheus_task {
            task.abort();
        }
    }
}

/// Install the global telemetry pipeline. Idempotent and infallible from the
/// caller's perspective: a failed exporter build degrades to a warning and a
/// reduced pipeline; an already-installed subscriber degrades to a no-op.
///
/// When [`OtelConfig`] requests export, call this inside a Tokio runtime
/// context — the OTLP exporters and Prometheus server run on it.
pub fn init(cfg: &TelemetryConfig, role: ServiceRole) -> TelemetryGuard {
    // `RUST_LOG` wins, then the configured level, then a safe default.
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&cfg.level))
        .unwrap_or_else(|_| EnvFilter::new("info"));

    // All roles log to stderr. This is required for `McpStdio`, whose stdout is
    // the protocol channel — telemetry must never write there.
    let fmt_layer = match cfg.format {
        LogFormat::Json => fmt::layer().json().with_writer(std::io::stderr).boxed(),
        LogFormat::Text => fmt::layer().with_writer(std::io::stderr).boxed(),
    };

    let mut layers: Vec<Box<dyn Layer<Registry> + Send + Sync>> = vec![fmt_layer];
    let mut guard = TelemetryGuard::default();

    if let Some(otel) = &cfg.otel {
        let resource = Resource::builder()
            .with_service_name(otel.service_name.clone())
            .with_attribute(KeyValue::new("service.role", role.as_str()))
            .build();

        if otel.traces
            && let Some(layer) = build_trace_layer(otel, &resource, &mut guard)
        {
            layers.push(layer);
        }
        if otel.logs
            && let Some(layer) = build_log_layer(otel, &resource, &mut guard)
        {
            layers.push(layer);
        }
        init_metrics(otel, &resource, &mut guard);

        // Cross-process correlation rides on W3C `traceparent`.
        opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
    }

    let installed = Registry::default()
        .with(layers)
        .with(filter)
        .try_init()
        .is_ok();
    if installed {
        tracing::debug!(role = role.as_str(), "pawrly telemetry initialized");
    }

    guard
}

/// Build the OTLP span exporter + tracer provider and return a
/// `tracing-opentelemetry` layer wired to it. Returns `None` (after a warning)
/// if the exporter cannot be built.
fn build_trace_layer(
    otel: &OtelConfig,
    resource: &Resource,
    guard: &mut TelemetryGuard,
) -> Option<Box<dyn Layer<Registry> + Send + Sync>> {
    let builder = opentelemetry_otlp::SpanExporter::builder();
    let built = match otel.protocol {
        OtelProtocol::Grpc => builder.with_tonic().with_endpoint(&otel.endpoint).build(),
        OtelProtocol::Http => builder
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(&otel.endpoint)
            .build(),
    };
    let exporter = match built {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "OTLP trace exporter disabled: build failed");
            return None;
        }
    };

    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        // Parent-based so a sampled upstream span keeps its whole subtree;
        // the ratio bounds export volume for locally-rooted traces.
        .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
            otel.sample_ratio,
        ))))
        .with_resource(resource.clone())
        .build();

    let tracer = provider.tracer(SCOPE);
    opentelemetry::global::set_tracer_provider(provider.clone());
    guard.tracer_provider = Some(provider);

    Some(tracing_opentelemetry::layer().with_tracer(tracer).boxed())
}

/// Build the OTLP log exporter + logger provider and return the appender bridge
/// layer that forwards `tracing` events to it. Returns `None` (after a warning)
/// if the exporter cannot be built.
fn build_log_layer(
    otel: &OtelConfig,
    resource: &Resource,
    guard: &mut TelemetryGuard,
) -> Option<Box<dyn Layer<Registry> + Send + Sync>> {
    let builder = opentelemetry_otlp::LogExporter::builder();
    let built = match otel.protocol {
        OtelProtocol::Grpc => builder.with_tonic().with_endpoint(&otel.endpoint).build(),
        OtelProtocol::Http => builder
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(&otel.endpoint)
            .build(),
    };
    let exporter = match built {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "OTLP log exporter disabled: build failed");
            return None;
        }
    };

    let provider = SdkLoggerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource.clone())
        .build();

    let bridge = opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(&provider);
    guard.logger_provider = Some(provider);

    // Drop `opentelemetry`-targeted events: exporting a log can itself emit one,
    // which would otherwise feed back into the exporter.
    let no_feedback =
        tracing_subscriber::filter::filter_fn(|meta| !meta.target().starts_with("opentelemetry"));
    Some(bridge.with_filter(no_feedback).boxed())
}

/// Build the meter provider from the configured readers (OTLP push and/or
/// Prometheus pull) and install it globally. A no-op when neither is enabled.
fn init_metrics(otel: &OtelConfig, resource: &Resource, guard: &mut TelemetryGuard) {
    let mut builder = SdkMeterProvider::builder().with_resource(resource.clone());
    let mut enabled = false;

    if otel.metrics
        && let Some(exporter) = build_metric_exporter(otel)
    {
        builder = builder.with_reader(PeriodicReader::builder(exporter).build());
        enabled = true;
    }

    let mut prometheus = None;
    if let Some(prom) = &otel.prometheus {
        let registry = prometheus::Registry::new();
        match opentelemetry_prometheus::exporter()
            .with_registry(registry.clone())
            .build()
        {
            Ok(reader) => {
                builder = builder.with_reader(reader);
                prometheus = Some((registry, prom.listen));
                enabled = true;
            }
            Err(e) => tracing::warn!(error = %e, "Prometheus exporter disabled: build failed"),
        }
    }

    if !enabled {
        return;
    }

    let provider = builder.build();
    opentelemetry::global::set_meter_provider(provider.clone());
    guard.meter_provider = Some(provider);

    if let Some((registry, listen)) = prometheus {
        guard.prometheus_task = Some(spawn_metrics_server(registry, listen));
    }
}

/// Build the OTLP metric exporter for the configured protocol, warning and
/// returning `None` on failure.
fn build_metric_exporter(otel: &OtelConfig) -> Option<opentelemetry_otlp::MetricExporter> {
    let builder = opentelemetry_otlp::MetricExporter::builder();
    let built = match otel.protocol {
        OtelProtocol::Grpc => builder.with_tonic().with_endpoint(&otel.endpoint).build(),
        OtelProtocol::Http => builder
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(&otel.endpoint)
            .build(),
    };
    match built {
        Ok(e) => Some(e),
        Err(e) => {
            tracing::warn!(error = %e, "OTLP metric exporter disabled: build failed");
            None
        }
    }
}

/// Serve the Prometheus registry as a `/metrics` text endpoint until the guard
/// is dropped (which aborts this task).
fn spawn_metrics_server(
    registry: prometheus::Registry,
    listen: SocketAddr,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let handler = move || {
            let registry = registry.clone();
            async move {
                let mut buf = String::new();
                let families = registry.gather();
                match prometheus::TextEncoder::new().encode_utf8(&families, &mut buf) {
                    Ok(()) => buf,
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to encode Prometheus metrics");
                        String::new()
                    }
                }
            }
        };
        let app = axum::Router::new().route("/metrics", axum::routing::get(handler));
        match tokio::net::TcpListener::bind(listen).await {
            Ok(listener) => {
                if let Err(e) = axum::serve(listener, app).await {
                    tracing::warn!(error = %e, "Prometheus /metrics server stopped");
                }
            }
            Err(e) => tracing::warn!(error = %e, addr = %listen, "Prometheus /metrics bind failed"),
        }
    })
}
