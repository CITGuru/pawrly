//! Runtime descriptors for sources, used by the engine and gRPC layers.
//!
//! `pawrly-config` produces these from YAML; the engine consumes them.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::cache::CachePolicy;
use crate::model::SourceKind;
use crate::safety::SafetyPolicy;

/// Engine-side description of a single source. The `config` blob is opaque
/// JSON; it's interpreted by the per-kind `SourceBuilder`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceDef {
    pub name: String,
    pub kind: SourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Agent-facing usage notes for the whole source, surfaced through
    /// `describe_table` alongside any per-table `wiki`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wiki: Option<String>,
    /// SQL statements that must run successfully against this source; run by
    /// `pawrly check` and surfaced through `describe_table`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
    /// Raw per-kind config tree. Validated by the source builder.
    pub config: serde_json::Value,
    #[serde(default)]
    pub cache: CachePolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety: Option<SafetyPolicy>,
    /// Per-table overrides (typed tables for HTTP, etc.).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tables: Vec<TableDef>,
    /// For HTTP-shaped sources: register a raw-HTTP table named after the source.
    #[serde(default)]
    pub raw_table: bool,
    /// Safety policy for the raw-HTTP table when `raw_table = true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_table_safety: Option<SafetyPolicy>,
}

/// Per-table override / declaration. The shape of the per-kind body is
/// kind-specific and lives in `config`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableDef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Agent-facing usage notes, surfaced through `describe_table`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wiki: Option<String>,
    /// Per-kind opaque config (endpoint, params, schema, query, path, …).
    #[serde(default = "default_value", skip_serializing_if = "is_null")]
    pub config: serde_json::Value,
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

/// Status of a registered source, surfaced via `EngineService::list_sources`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceStatus {
    Ok,
    Unavailable,
}

/// Lightweight info row about a registered source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceInfo {
    pub name: String,
    pub kind: SourceKind,
    pub status: SourceStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_detail: Option<String>,
    /// Finer-grained variant of `kind` for display, when the bare kind hides a
    /// meaningful mode — e.g. `"openapi"` for a spec-driven HTTP source, or
    /// `"object_storage"` for a file source backed by a remote object store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_kind: Option<String>,
    pub table_count: u64,
    pub registered_at: DateTime<Utc>,
}

/// Result of `EngineService::test_source`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceTestReport {
    pub name: String,
    pub ok: bool,
    #[serde(with = "humantime_serde")]
    pub latency: Duration,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Result of `EngineService::reload_config`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReloadReport {
    pub sources_added: u64,
    pub sources_removed: u64,
    pub sources_changed: u64,
}

/// Result of `EngineService::refresh_catalog`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RefreshCatalogOutcome {
    pub sources_refreshed: u64,
    pub tables_discovered: u64,
}

/// Health of the engine; surfaced via `EngineService::health`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthReport {
    pub ok: bool,
    pub version: String,
    pub active_queries: u64,
    pub sources_ok: u64,
    pub sources_unavailable: u64,
}
