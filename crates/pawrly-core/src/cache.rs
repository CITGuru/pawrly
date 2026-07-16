//! Cache policy types. The actual cache implementation lives in `pawrly-engine`.

use std::time::Duration;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::schema::TableName;

/// Per-table cache mode declared in YAML.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum CachePolicy {
    /// Default. No caching; every query hits the live source.
    #[default]
    None,

    /// Read from the cache if fresher than `ttl`; else fetch live and write through.
    Ttl {
        #[serde(with = "humantime_serde")]
        #[schemars(with = "String")]
        ttl: Duration,
    },

    /// Always read from the cache; a background task re-fetches every `every`.
    Refresh {
        #[serde(with = "humantime_serde")]
        #[schemars(with = "String")]
        every: Duration,
    },

    /// Same as `refresh`, scheduled by cron expression.
    Cron { cron: String },

    /// Incremental: a `cursor_column` is used to fetch only newer rows on each refresh.
    Append { cursor_column: String },
}

impl CachePolicy {
    /// Convenience accessor: `Some(duration)` for modes that have a refresh interval.
    #[must_use]
    pub fn refresh_interval(&self) -> Option<Duration> {
        match self {
            Self::Refresh { every } => Some(*every),
            _ => None,
        }
    }

    /// Whether the cache layer should treat reads as cacheable at all.
    #[must_use]
    pub fn caches(&self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Aliases for the gRPC enum form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheMode {
    None,
    Ttl,
    Refresh,
    Cron,
    Append,
    Pinned,
}

impl From<&CachePolicy> for CacheMode {
    fn from(p: &CachePolicy) -> Self {
        match p {
            CachePolicy::None => Self::None,
            CachePolicy::Ttl { .. } => Self::Ttl,
            CachePolicy::Refresh { .. } => Self::Refresh,
            CachePolicy::Cron { .. } => Self::Cron,
            CachePolicy::Append { .. } => Self::Append,
        }
    }
}

/// One row of the cache manifest, surfaced via `EngineService::cache_entries`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntryInfo {
    pub name: TableName,
    pub mode: CacheMode,
    pub written_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    pub row_count: u64,
    pub size_bytes: u64,
    pub file_count: u32,
}

/// Result of `EngineService::refresh_table`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshOutcome {
    pub table: TableName,
    pub rows_written: u64,
    pub size_bytes: u64,
    #[serde(with = "humantime_serde")]
    pub elapsed: Duration,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

/// Result of `EngineService::vacuum_cache`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VacuumReport {
    pub entries_removed: u64,
    pub files_removed: u64,
    pub bytes_reclaimed: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ttl_humantime() {
        let yaml = r#"
mode: ttl
ttl: 10m
"#;
        let p: CachePolicy = serde_yaml::from_str(yaml).unwrap();
        match p {
            CachePolicy::Ttl { ttl } => assert_eq!(ttl, Duration::from_secs(600)),
            _ => unreachable!(),
        }
    }

    #[test]
    fn default_is_none() {
        let yaml = r#"
mode: none
"#;
        let p: CachePolicy = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(p, CachePolicy::None);
        assert!(!p.caches());
    }
}
