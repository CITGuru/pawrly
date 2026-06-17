//! The `observability:` config block: logging, OpenTelemetry export, and the
//! activity log. An absent block means today's behaviour. CLI flags override
//! the `tracing:`/`otel:` settings.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The `observability:` block.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct ObservabilityConfig {
    /// Logging level and format.
    pub tracing: TracingConfig,
    /// OpenTelemetry export (OTLP push and/or Prometheus pull).
    pub otel: OtelConfig,
    /// Activity log settings.
    pub activity: ActivityConfig,
}

/// The `observability.tracing:` block.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct TracingConfig {
    /// `EnvFilter` directive (e.g. `info`, `pawrly=debug`). `RUST_LOG` wins.
    pub level: String,
    /// Log line format.
    pub format: LogFormat,
}

impl Default for TracingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            format: LogFormat::Text,
        }
    }
}

/// Log line format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Human-readable text.
    #[default]
    Text,
    /// Line-delimited JSON.
    Json,
}

/// The `observability.otel:` block.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct OtelConfig {
    /// Master switch for OTLP export.
    pub enabled: bool,
    /// Collector endpoint.
    pub endpoint: String,
    /// OTLP transport.
    pub protocol: OtelProtocol,
    /// `service.name` resource attribute.
    pub service_name: String,
    /// Export traces.
    pub traces: bool,
    /// Export metrics over OTLP push.
    pub metrics: bool,
    /// Bridge logs to OTel and export them.
    pub logs: bool,
    /// Parent-based ratio sampler probability in `[0.0, 1.0]`.
    pub sample_ratio: f64,
    /// Prometheus pull endpoint, independent of OTLP push.
    pub prometheus: PrometheusConfig,
}

impl Default for OtelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: "http://localhost:4317".to_string(),
            protocol: OtelProtocol::Grpc,
            service_name: "pawrly".to_string(),
            traces: true,
            metrics: true,
            logs: true,
            sample_ratio: 1.0,
            prometheus: PrometheusConfig::default(),
        }
    }
}

/// OTLP transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OtelProtocol {
    /// OTLP over gRPC.
    #[default]
    Grpc,
    /// OTLP over HTTP/protobuf.
    Http,
}

/// The `observability.otel.prometheus:` block.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct PrometheusConfig {
    /// Serve a `/metrics` pull endpoint.
    pub enabled: bool,
    /// Address the endpoint listens on.
    pub listen: String,
}

impl Default for PrometheusConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: "127.0.0.1:9090".to_string(),
        }
    }
}

/// The `observability.activity:` block — one structured record per operation.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct ActivityConfig {
    /// Master switch. Off by default, so no records are produced.
    pub enabled: bool,
    /// Which sinks receive records. Defaults to the `tracing` event sink.
    pub sinks: Vec<ActivitySinkKind>,
    /// How much of the submitted SQL to capture.
    #[schemars(with = "String")]
    pub redact_sql: RedactSql,
    /// In-memory ring-buffer capacity for the `table` sink.
    pub ring_capacity: usize,
    /// Durable store directory for the `table` sink. Omit for in-memory only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<PathBuf>,
}

impl Default for ActivityConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            sinks: vec![ActivitySinkKind::Tracing],
            redact_sql: RedactSql::Off,
            ring_capacity: 10_000,
            store: None,
        }
    }
}

/// An activity sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActivitySinkKind {
    /// Emit each record as a `tracing` event (sink 1).
    Tracing,
    /// Expose records via the `system.activity` table (sink 2).
    Table,
}

/// SQL capture policy. Accepts `false`/`true` (booleans) or `off`/`literals`/
/// `tables` (strings) in YAML, matching the design doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RedactSql {
    /// Store SQL verbatim. (`false`)
    #[default]
    Off,
    /// Replace literal values with a sentinel, keeping shape. (`literals`)
    Literals,
    /// Store only the statement kind and tables. (`true`)
    Tables,
}

impl<'de> Deserialize<'de> for RedactSql {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Bool(bool),
            Str(String),
        }
        match Raw::deserialize(deserializer)? {
            Raw::Bool(false) => Ok(RedactSql::Off),
            Raw::Bool(true) => Ok(RedactSql::Tables),
            Raw::Str(s) => match s.as_str() {
                "false" | "off" => Ok(RedactSql::Off),
                "literals" => Ok(RedactSql::Literals),
                "true" | "tables" => Ok(RedactSql::Tables),
                other => Err(serde::de::Error::custom(format!(
                    "invalid redact_sql `{other}`: expected false | literals | true"
                ))),
            },
        }
    }
}

impl Serialize for RedactSql {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(match self {
            RedactSql::Off => "false",
            RedactSql::Literals => "literals",
            RedactSql::Tables => "true",
        })
    }
}
