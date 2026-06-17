//! Pawrly observability.
//!
//! Two surfaces, split so the *emit* side stays dependency-light:
//! - [`metrics`] — a thin instrument facade over the global OTel meter, needing
//!   only the lightweight `opentelemetry` API. The engine and source crates
//!   depend on this crate with `default-features = false` to emit metrics
//!   without pulling the exporter tree.
//! - `init` (the default `otel` feature) — installs the global pipeline:
//!   `tracing-subscriber` plus, when configured, OTLP trace/log/metric
//!   exporters, a Prometheus pull endpoint, and the W3C propagator. Binaries
//!   call [`init`] once and hold the returned [`TelemetryGuard`].
//!
//! Everything is no-op until [`init`] runs, so absent configuration means
//! today's behaviour.

pub mod metrics;

#[cfg(feature = "otel")]
mod otel;

#[cfg(feature = "otel")]
pub use otel::{
    LogFormat, OtelConfig, OtelProtocol, PrometheusConfig, ServiceRole, TelemetryConfig,
    TelemetryGuard, init,
};
