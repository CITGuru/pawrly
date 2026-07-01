//! Top-level config types parsed from `pawrly.yaml`.
//!
//! The parsed `Config` is converted into a list of `pawrly_core::SourceDef`
//! for the engine.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use pawrly_core::semantic::SemanticModel;
use pawrly_core::{
    CachePolicy, FunctionArg, FunctionColumn, FunctionDef, FunctionKind, SafetyPolicy, SourceKind,
};

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

    /// Global (workspace-scoped) variable declarations, visible to every
    /// source. Per-source and fragment-file scopes layer on top during load.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub variables: std::collections::BTreeMap<String, crate::variables::VariableDef>,

    /// Declared sources.
    #[serde(default)]
    pub sources: Vec<SourceDef>,

    /// Declared standalone table-valued functions (top-level `functions:`
    /// block). Each carries an explicit `namespace`, `kind`, and `config`.
    /// Source-attached functions live under their source's `functions:` instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub functions: Vec<FunctionDecl>,

    /// Optional semantic layer. Absent = the layer is off and behavior is
    /// unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic: Option<SemanticConfig>,

    /// Optional observability block. Absent = today's behaviour (no activity
    /// log; export controlled by CLI flags).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observability: Option<crate::observability::ObservabilityConfig>,
}

fn default_name() -> String {
    "default".to_string()
}

/// The `semantic:` config block — a set of business-named models.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SemanticConfig {
    /// Files (or glob patterns) that contain *only* semantic models, merged
    /// into `models` before validation. Each file is either a top-level
    /// `models:` list or a bare YAML sequence of model mappings — never sources,
    /// secrets, or other config. Resolved relative to the declaring file. The
    /// loader consumes this during multi-file assembly, so it is always empty on
    /// a fully-loaded `Config`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,

    #[serde(default)]
    pub models: Vec<SemanticModel>,
}

/// One secret backend in the resolution chain.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum SecretsBackendDef {
    /// Process environment.
    Env,
    /// A file on disk — a YAML map or a dotenv (`.env`) file. The `format`
    /// defaults to auto-detection from the file extension. Relative paths
    /// resolve against the directory of the declaring config file.
    File {
        path: String,
        #[serde(default)]
        format: SecretsFileFormat,
    },
    /// OS keyring under the given service.
    Keyring {
        #[serde(default = "default_keyring_service")]
        service: String,
    },
    /// Convenience chain: process environment, then the OS keyring (service
    /// `pawrly`), then a `.env` file in the config directory if one exists.
    /// A missing or insecure `.env` is skipped (with a warning), never fatal.
    Auto,
}

/// On-disk format of a [`SecretsBackendDef::File`] backend.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SecretsFileFormat {
    /// Detect from the file extension (`.env` → dotenv, else YAML).
    #[default]
    Auto,
    /// A YAML map of `KEY: value`.
    Yaml,
    /// Dotenv-style `KEY=value` lines.
    Dotenv,
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

    /// Agent-facing usage notes for the whole source, surfaced through
    /// `describe_table` alongside any per-table `wiki`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wiki: Option<String>,

    /// SQL statements that must run successfully against this source. Run as
    /// live probes by `pawrly check` and surfaced through `describe_table`
    /// for the tables they reference.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,

    /// Optional path to a YAML file containing the rest of this source's body.
    /// Resolved relative to the declaring file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,

    /// Source-local variable declarations, visible to this source only
    /// (innermost in the scope chain). Resolved into `config`/`tables` at load.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub variables: std::collections::BTreeMap<String, crate::variables::VariableDef>,

    /// Bindings for this source's *dynamic* `${var:}` references, emitted during
    /// load (the static ones are inlined into `config`). Not (de)serialized —
    /// recomputed on every load — and threaded to the engine to build the
    /// variable store and the per-source `NAME → VarId` map.
    #[serde(skip)]
    #[schemars(skip)]
    pub dynamic_vars: Vec<pawrly_core::DynamicVarBinding>,

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

    /// Source-attached table-valued functions. They inherit this source's
    /// namespace (its name), `kind`, and connection `config`, so they omit
    /// `namespace`/`kind`/`config` of their own. Only valid on http/mcp/file
    /// sources.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub functions: Vec<FunctionDecl>,

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

    /// Agent-facing usage notes, surfaced through `describe_table` alongside
    /// the schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wiki: Option<String>,

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

/// One function declaration — the single config wrapper for both the
/// source-attached and standalone shapes.
///
/// Like [`TableDef`], `deny_unknown_fields` is intentionally absent: the
/// `#[serde(flatten)] body` captures every kind-specific key (`endpoint`,
/// `response`, `pagination`, `path`, `tool`, `rows_path`, ...). Top-level
/// entries require `namespace` + `kind`; source-attached entries omit both plus
/// `config` (all inherited). Placement and content are checked by the validator.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FunctionDecl {
    /// Function name; a valid SQL identifier with no `__`.
    pub name: String,

    /// SQL qualifier — **standalone only**. Source-attached functions inherit
    /// the source name as their namespace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,

    /// Execution backend — **standalone only**. Attached functions inherit it
    /// from the source kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<FunctionKind>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wiki: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,

    /// Ordered argument declarations — list order is the positional call order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<FunctionArg>,

    /// Output columns; the schema is fixed at plan time. Non-empty (validated).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub returns: Vec<FunctionColumn>,

    /// Standalone connection config (same shape as the matching source kind's
    /// `config`). Forbidden on attached functions.
    #[serde(default = "default_value", skip_serializing_if = "is_null")]
    pub config: serde_json::Value,

    /// Kind-specific body, flattened so its keys live at the top level of the
    /// function block (`endpoint`, `response`, `path`, `tool`, ...).
    #[serde(flatten)]
    pub body: serde_json::Value,

    /// Reserved; cache is inert in v1.
    #[serde(default)]
    pub cache: CachePolicy,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety: Option<SafetyPolicy>,
}

impl FunctionDecl {
    /// Resolve into the engine-facing [`FunctionDef`] given the effective
    /// `namespace`, `kind`, `connection`, and parent `source` (the latter two
    /// inherited for attached functions, explicit for standalone).
    fn to_engine(
        &self,
        namespace: String,
        kind: FunctionKind,
        connection: serde_json::Value,
        source: Option<String>,
    ) -> FunctionDef {
        FunctionDef {
            namespace,
            name: self.name.clone(),
            kind,
            description: self.description.clone(),
            wiki: self.wiki.clone(),
            examples: self.examples.clone(),
            args: self.args.clone(),
            returns: self.returns.clone(),
            connection,
            body: self.body.clone(),
            source,
            builtin: false,
            cache: self.cache.clone(),
            safety: self.safety.clone(),
        }
    }
}

impl Config {
    /// Union of every source's dynamic-variable specs, keyed by `VarId`. The
    /// engine builds the runtime variable store from this map.
    #[must_use]
    pub fn dynamic_specs(
        &self,
    ) -> std::collections::HashMap<pawrly_core::VarId, pawrly_core::DynamicVarSpec> {
        let mut out = std::collections::HashMap::new();
        for s in &self.sources {
            for b in &s.dynamic_vars {
                out.insert(b.id.clone(), b.spec.clone());
            }
        }
        out
    }

    /// Per-source `NAME → VarId` map, for sources that reference any dynamic
    /// variable. The source registrar uses it to resolve `${var:}` placeholders.
    #[must_use]
    pub fn dynamic_bindings_by_source(
        &self,
    ) -> std::collections::HashMap<String, std::collections::HashMap<String, pawrly_core::VarId>>
    {
        let mut out = std::collections::HashMap::new();
        for s in &self.sources {
            if s.dynamic_vars.is_empty() {
                continue;
            }
            let map = s
                .dynamic_vars
                .iter()
                .map(|b| (b.name.clone(), b.id.clone()))
                .collect();
            out.insert(s.name.clone(), map);
        }
        out
    }

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
                wiki: s.wiki,
                examples: s.examples,
                config: s.config,
                cache: s.cache,
                safety: s.safety,
                tables: s
                    .tables
                    .into_iter()
                    .map(|t| pawrly_core::TableDef {
                        name: t.name,
                        description: t.description,
                        wiki: t.wiki,
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

    /// Resolve declared functions (source-attached + standalone) into
    /// engine-facing descriptors. Taken by `&self` so it runs before
    /// [`Config::into_engine_sources`] consumes the config. Attached functions
    /// inherit their source's namespace, kind, and `config`; standalone use their
    /// explicit fields. Assumes validation has passed: an attached function on a
    /// non-http/mcp/file source is skipped.
    #[must_use]
    pub fn engine_functions(&self) -> Vec<FunctionDef> {
        let mut out = Vec::new();
        for src in &self.sources {
            let Some(kind) = FunctionKind::for_source(src.kind) else {
                continue;
            };
            for f in &src.functions {
                out.push(f.to_engine(
                    src.name.clone(),
                    kind,
                    src.config.clone(),
                    Some(src.name.clone()),
                ));
            }
        }
        for f in &self.functions {
            let (Some(namespace), Some(kind)) = (f.namespace.clone(), f.kind) else {
                continue;
            };
            out.push(f.to_engine(namespace, kind, f.config.clone(), None));
        }
        out
    }
}
