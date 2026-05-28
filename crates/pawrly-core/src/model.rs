//! Source kind enumeration. The set of values a user may put in `kind:` in YAML.
//!
//! The user-facing alias (string form) is what flows through configs and gRPC;
//! the enum is the engine-internal canonical representation.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Every supported `kind:` for a source declaration.
///
/// This list is closed: adding a new kind requires a code change so the
/// router can dispatch it. Kind aliases are matched case-insensitively at
/// parse time but the canonical form is lowercase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    // HTTP-shaped
    Http,
    Github,
    Linear,
    Stripe,
    Sentry,
    Datadog,
    Slack,
    Notion,
    // AI
    Ai,
    // DuckDB-backed
    File,
    Postgres,
    Mysql,
    Sqlite,
    Snowflake,
    Bigquery,
    Redshift,
    Iceberg,
    Delta,
    S3,
    Gcs,
    Azure,
}

impl SourceKind {
    /// String form used in YAML and on the gRPC wire.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Github => "github",
            Self::Linear => "linear",
            Self::Stripe => "stripe",
            Self::Sentry => "sentry",
            Self::Datadog => "datadog",
            Self::Slack => "slack",
            Self::Notion => "notion",
            Self::Ai => "ai",
            Self::File => "file",
            Self::Postgres => "postgres",
            Self::Mysql => "mysql",
            Self::Sqlite => "sqlite",
            Self::Snowflake => "snowflake",
            Self::Bigquery => "bigquery",
            Self::Redshift => "redshift",
            Self::Iceberg => "iceberg",
            Self::Delta => "delta",
            Self::S3 => "s3",
            Self::Gcs => "gcs",
            Self::Azure => "azure",
        }
    }

    /// True if this source kind speaks REST/HTTP and is implemented as a
    /// pure-Rust DataFusion `TableProvider`.
    #[must_use]
    pub fn is_http_shaped(&self) -> bool {
        matches!(
            self,
            Self::Http
                | Self::Github
                | Self::Linear
                | Self::Stripe
                | Self::Sentry
                | Self::Datadog
                | Self::Slack
                | Self::Notion
        )
    }

    /// True if this source kind is implemented through DuckDB extensions.
    #[must_use]
    pub fn is_duckdb_backed(&self) -> bool {
        matches!(
            self,
            Self::File
                | Self::Postgres
                | Self::Mysql
                | Self::Sqlite
                | Self::Snowflake
                | Self::Bigquery
                | Self::Redshift
                | Self::Iceberg
                | Self::Delta
                | Self::S3
                | Self::Gcs
                | Self::Azure
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
            "github" => Ok(Self::Github),
            "linear" => Ok(Self::Linear),
            "stripe" => Ok(Self::Stripe),
            "sentry" => Ok(Self::Sentry),
            "datadog" => Ok(Self::Datadog),
            "slack" => Ok(Self::Slack),
            "notion" => Ok(Self::Notion),
            "ai" => Ok(Self::Ai),
            "file" => Ok(Self::File),
            "postgres" | "pg" | "postgresql" => Ok(Self::Postgres),
            "mysql" => Ok(Self::Mysql),
            "sqlite" => Ok(Self::Sqlite),
            "snowflake" => Ok(Self::Snowflake),
            "bigquery" | "bq" => Ok(Self::Bigquery),
            "redshift" => Ok(Self::Redshift),
            "iceberg" => Ok(Self::Iceberg),
            "delta" | "deltalake" => Ok(Self::Delta),
            "s3" => Ok(Self::S3),
            "gcs" | "gs" => Ok(Self::Gcs),
            "azure" | "abfs" => Ok(Self::Azure),
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
            SourceKind::Github,
            SourceKind::Snowflake,
            SourceKind::Iceberg,
            SourceKind::Ai,
            SourceKind::File,
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
        let bq: SourceKind = "bq".parse().unwrap();
        assert_eq!(bq, SourceKind::Bigquery);
    }

    #[test]
    fn classification() {
        assert!(SourceKind::Github.is_http_shaped());
        assert!(SourceKind::Snowflake.is_duckdb_backed());
        assert!(!SourceKind::Ai.is_http_shaped());
        assert!(!SourceKind::Ai.is_duckdb_backed());
    }

    #[test]
    fn unknown_errs() {
        let e: Result<SourceKind, _> = "flubber".parse();
        assert!(e.is_err());
    }
}
