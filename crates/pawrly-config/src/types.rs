//! Top-level config types parsed from `pawrly.yaml`.
//!
//! The parsed `Config` is converted into a list of `pawrly_core::SourceDef`
//! for the engine.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use pawrly_core::{CachePolicy, SafetyPolicy, SourceKind};

use crate::defaults::Defaults;

/// Top-level workspace config.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Schema version. Only `1` is accepted in v1.
    pub version: u32,

    /// Workspace name. Defaults to `"default"`.
    #[serde(default = "default_name")]
    pub name: String,

    /// Workspace defaults.
    #[serde(default)]
    pub defaults: Defaults,

    /// Configured secret backends. Empty = use the built-in default chain
    /// (`env`, then OS keyring under `service=pawrly`).
    #[serde(default)]
    pub secrets: Vec<SecretsBackendDef>,

    /// Files (or glob patterns) whose `sources:` and optional `secrets:` are
    /// merged into this config before validation. Resolved relative to the
    /// declaring file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,

    /// Declared sources.
    #[serde(default)]
    pub sources: Vec<SourceDef>,
}

fn default_name() -> String {
    "default".to_string()
}

/// One secret backend in the resolution chain.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum SecretsBackendDef {
    /// Process environment.
    Env,
    /// YAML file on disk.
    File { path: String },
    /// OS keyring under the given service.
    Keyring {
        #[serde(default = "default_keyring_service")]
        service: String,
    },
}

fn default_keyring_service() -> String {
    "pawrly".to_string()
}

/// One source declaration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceDef {
    pub name: String,
    pub kind: SourceKind,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Optional path to a YAML file containing the rest of this source's body.
    /// Resolved relative to the declaring file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,

    /// Per-kind opaque config. Validated by the source builder when registered.
    #[serde(default = "default_value", skip_serializing_if = "is_null")]
    pub config: serde_json::Value,

    /// Workspace-level cache override for this source.
    #[serde(default)]
    pub cache: CachePolicy,

    /// Workspace-level safety override for this source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety: Option<SafetyPolicy>,

    /// Per-table declarations / overrides.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tables: Vec<TableDef>,

    /// HTTP-shaped sources only: register a raw-HTTP table named after the source.
    #[serde(default)]
    pub raw_table: bool,

    /// HTTP-shaped sources only: safety policy for the raw table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_table_safety: Option<SafetyPolicy>,
}

/// One per-table override / declaration. The body is opaque per-kind.
///
/// `deny_unknown_fields` is intentionally absent — this struct uses
/// `#[serde(flatten)] body` to capture every kind-specific key (endpoint,
/// path, format, params, …), so unknown fields are *expected* and routed
/// into `body`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TableDef {
    pub name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Per-kind opaque body (endpoint, params, schema, query, path, format, …).
    /// Flattened so YAML keys live at the top level of the table block.
    #[serde(flatten)]
    pub body: serde_json::Value,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<CachePolicy>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety: Option<SafetyPolicy>,
}

fn default_value() -> serde_json::Value {
    serde_json::Value::Null
}

fn is_null(v: &serde_json::Value) -> bool {
    v.is_null()
}

impl Config {
    /// Convert into engine-side runtime descriptors. Caller has already
    /// resolved secrets and validated.
    #[must_use]
    pub fn into_engine_sources(self) -> Vec<pawrly_core::SourceDef> {
        self.sources
            .into_iter()
            .map(|s| pawrly_core::SourceDef {
                name: s.name,
                kind: s.kind,
                description: s.description,
                config: s.config,
                cache: s.cache,
                safety: s.safety,
                tables: s
                    .tables
                    .into_iter()
                    .map(|t| pawrly_core::TableDef {
                        name: t.name,
                        description: t.description,
                        config: t.body,
                        cache: t.cache,
                        safety: t.safety,
                    })
                    .collect(),
                raw_table: s.raw_table,
                raw_table_safety: s.raw_table_safety,
            })
            .collect()
    }
}
