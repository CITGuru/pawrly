//! Top-level `defaults:` block in `pawrly.yaml`.

use std::path::PathBuf;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use pawrly_core::CachePolicy;

/// Workspace-level defaults that apply unless overridden per source / table.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Defaults {
    pub cache: CacheDefaults,
    pub http: HttpDefaults,
    pub safety: SafetyDefaults,
    pub engine: EngineDefaults,
    pub optimizer: OptimizerDefaults,
}

/// Cache section under `defaults:`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct CacheDefaults {
    /// Filesystem path used as the cache root. `~/.pawrly/cache` by default.
    pub storage: PathBuf,
    /// Default cache mode applied to tables that don't declare their own.
    #[serde(default)]
    pub mode: CachePolicy,
}

impl Default for CacheDefaults {
    fn default() -> Self {
        Self {
            storage: PathBuf::from("~/.pawrly/cache"),
            mode: CachePolicy::None,
        }
    }
}

/// HTTP section under `defaults:`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct HttpDefaults {
    #[serde(with = "humantime_serde")]
    #[schemars(with = "String")]
    pub timeout: Duration,
    pub user_agent: String,
}

impl Default for HttpDefaults {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            user_agent: format!("pawrly/{}", env!("CARGO_PKG_VERSION")),
        }
    }
}

/// Safety section under `defaults:`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SafetyDefaults {
    /// Hard cap on returned rows for any source that doesn't declare its own.
    pub max_unfiltered_rows: u64,
}

impl Default for SafetyDefaults {
    fn default() -> Self {
        Self {
            max_unfiltered_rows: 1_000_000,
        }
    }
}

/// Engine section under `defaults:`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct EngineDefaults {
    /// Memory limit passed to DataFusion's memory pool. `None` = unlimited.
    pub memory_limit_bytes: Option<u64>,
    /// Per-query timeout.
    #[serde(with = "humantime_serde")]
    #[schemars(with = "String")]
    pub query_timeout: Duration,
    /// Maximum concurrent queries served from a single engine instance.
    pub max_concurrent_queries: u32,
    /// Size of the DuckDB connection pool (in-memory).
    pub duckdb_pool_size: u32,
}

impl Default for EngineDefaults {
    fn default() -> Self {
        Self {
            memory_limit_bytes: None,
            query_timeout: Duration::from_secs(300),
            max_concurrent_queries: 16,
            duckdb_pool_size: 8,
        }
    }
}

/// Optimizer section under `defaults:`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct OptimizerDefaults {
    /// Off in v1; enabled once dynamic-filter pushdown is implemented.
    pub dynamic_filter_pushdown: bool,
    pub join_reorder: bool,
    pub coalesce_batches: bool,
}

impl Default for OptimizerDefaults {
    fn default() -> Self {
        Self {
            dynamic_filter_pushdown: false,
            join_reorder: true,
            coalesce_batches: true,
        }
    }
}
