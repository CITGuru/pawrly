//! Source kind enumeration. The set of values a user may put in `kind:` in YAML.
//!
//! The user-facing alias (string form) is what flows through configs and gRPC;
//! the enum is the engine-internal canonical representation.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Every supported `kind:` for a source declaration.
///
/// Two foundational backends — `file` (local files, or object storage via a
/// `storage:` block) and `http` (any REST/GraphQL API) — plus a small set of
/// first-class database / lakehouse builtins. The list is closed: adding a new
/// kind requires a code change so the router can dispatch it. Kind aliases are
/// matched case-insensitively at parse time but the canonical form is lowercase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    // Foundational backends
    Http,
    File,
    Mcp,
    // First-class builtins (DuckDB-backed)
    Sqlite,
    Postgres,
    Mysql,
    Duckdb,
    Snowflake,
    Iceberg,
    Ducklake,
    Delta,
}

impl SourceKind {
    /// String form used in YAML and on the gRPC wire.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::File => "file",
            Self::Mcp => "mcp",
            Self::Sqlite => "sqlite",
            Self::Postgres => "postgres",
            Self::Mysql => "mysql",
            Self::Duckdb => "duckdb",
            Self::Snowflake => "snowflake",
            Self::Iceberg => "iceberg",
            Self::Ducklake => "ducklake",
            Self::Delta => "delta",
        }
    }

    /// True if this source kind speaks REST/HTTP and is implemented as a
    /// pure-Rust DataFusion `TableProvider`.
    #[must_use]
    pub fn is_http_shaped(&self) -> bool {
        matches!(self, Self::Http)
    }

    /// True if this source kind is implemented through the DuckDB pool (attach,
    /// scan functions, or object-store reads). The `file` kind is DataFusion-
    /// backed for local paths and DuckDB-backed only when a `storage:` block is
    /// present, so that routing decision lives in the registry, not here.
    #[must_use]
    pub fn is_duckdb_backed(&self) -> bool {
        matches!(
            self,
            Self::Sqlite
                | Self::Postgres
                | Self::Mysql
                | Self::Duckdb
                | Self::Snowflake
                | Self::Iceberg
                | Self::Ducklake
                | Self::Delta
        )
    }
}

impl std::fmt::Display for SourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for SourceKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lowered = s.to_ascii_lowercase();
        match lowered.as_str() {
            "http" => Ok(Self::Http),
            "file" => Ok(Self::File),
            "mcp" => Ok(Self::Mcp),
            "sqlite" => Ok(Self::Sqlite),
            "postgres" | "pg" | "postgresql" => Ok(Self::Postgres),
            "mysql" => Ok(Self::Mysql),
            "duckdb" => Ok(Self::Duckdb),
            "snowflake" => Ok(Self::Snowflake),
            "iceberg" => Ok(Self::Iceberg),
            "ducklake" => Ok(Self::Ducklake),
            "delta" | "deltalake" => Ok(Self::Delta),
            other => Err(format!("unknown source kind `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for kind in [
            SourceKind::Http,
            SourceKind::File,
            SourceKind::Mcp,
            SourceKind::Sqlite,
            SourceKind::Postgres,
            SourceKind::Duckdb,
            SourceKind::Snowflake,
            SourceKind::Iceberg,
            SourceKind::Ducklake,
            SourceKind::Delta,
        ] {
            let s = kind.as_str();
            let parsed: SourceKind = s.parse().unwrap();
            assert_eq!(parsed, kind);
        }
    }

    #[test]
    fn aliases_resolve() {
        let pg: SourceKind = "postgresql".parse().unwrap();
        assert_eq!(pg, SourceKind::Postgres);
        let pg2: SourceKind = "pg".parse().unwrap();
        assert_eq!(pg2, SourceKind::Postgres);
        let delta: SourceKind = "deltalake".parse().unwrap();
        assert_eq!(delta, SourceKind::Delta);
    }

    #[test]
    fn classification() {
        assert!(SourceKind::Http.is_http_shaped());
        assert!(!SourceKind::File.is_http_shaped());
        assert!(SourceKind::Snowflake.is_duckdb_backed());
        assert!(SourceKind::Ducklake.is_duckdb_backed());
        assert!(!SourceKind::Http.is_duckdb_backed());
    }

    #[test]
    fn removed_kinds_error() {
        for s in ["github", "linear", "ai", "s3", "gcs", "azure", "bigquery"] {
            assert!(s.parse::<SourceKind>().is_err(), "{s} should be removed");
        }
    }

    #[test]
    fn unknown_errs() {
        let e: Result<SourceKind, _> = "flubber".parse();
        assert!(e.is_err());
    }
}
