//! YAML configuration loader, validator, and secret interpolation for Pawrly.

#![doc(html_root_url = "https://docs.rs/pawrly-config")]

mod assemble;
pub mod defaults;
pub mod interpolate;
pub mod loader;
pub mod observability;
pub mod schema;
pub mod secrets;
pub mod types;
pub mod validator;
pub mod variables;

pub use defaults::{Defaults, EngineDefaults, HttpDefaults, OptimizerDefaults, SafetyDefaults};
pub use loader::{
    IncludeNode, MaskedConfig, assemble_config, include_tree, load, load_auto, load_auto_with_vars,
    load_str, resolve_secret, secret_store, source_static_vars,
};
pub use observability::{
    ActivityConfig, ActivitySinkKind, LogFormat, ObservabilityConfig, OtelConfig, OtelProtocol,
    PrometheusConfig, RedactSql, TracingConfig,
};
pub use schema::json_schema;
pub use secrets::build_store;
pub use types::{
    Config, SecretsBackendDef, SecretsFileFormat, SemanticConfig, SourceDef as ConfigSourceDef,
    TableDef as ConfigTableDef,
};
pub use validator::validate;
pub use variables::{CredentialMethod, InputMethod, StaticVarRef, VarKind, VarType, VariableDef};
