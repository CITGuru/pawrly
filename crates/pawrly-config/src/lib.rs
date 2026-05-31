//! YAML configuration loader, validator, and secret interpolation for Pawrly.

#![doc(html_root_url = "https://docs.rs/pawrly-config")]

mod assemble;
pub mod defaults;
pub mod interpolate;
pub mod loader;
pub mod schema;
pub mod secrets;
pub mod types;
pub mod validator;

pub use defaults::{Defaults, EngineDefaults, HttpDefaults, OptimizerDefaults, SafetyDefaults};
pub use loader::{
    IncludeNode, MaskedConfig, assemble_config, include_tree, load, load_auto, load_str,
};
pub use schema::json_schema;
pub use secrets::build_store;
pub use types::{
    Config, SecretsBackendDef, SecretsFileFormat, SemanticConfig, SourceDef as ConfigSourceDef,
    TableDef as ConfigTableDef,
};
pub use validator::validate;
