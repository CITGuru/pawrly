//! YAML configuration loader, validator, and secret interpolation for Pawrly.

#![doc(html_root_url = "https://docs.rs/pawrly-config")]

mod assemble;
pub mod defaults;
pub mod interpolate;
pub mod loader;
pub mod schema;
pub mod types;
pub mod validator;

pub use defaults::{Defaults, EngineDefaults, HttpDefaults, OptimizerDefaults, SafetyDefaults};
pub use loader::{IncludeNode, MaskedConfig, assemble_config, include_tree, load, load_str};
pub use schema::json_schema;
pub use types::{
    Config, SecretsBackendDef, SourceDef as ConfigSourceDef, TableDef as ConfigTableDef,
};
pub use validator::validate;
