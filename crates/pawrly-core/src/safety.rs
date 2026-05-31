//! Safety policy: per-table guard rails enforced before scan execution.

use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Restrictions a source can place on its own tables to prevent accidental
/// "scan the entire warehouse" queries.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SafetyPolicy {
    /// Columns that must appear in a filter; empty means no requirement.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub require_filters_on: Vec<String>,

    /// If true, refuse to scan with no filter at all.
    #[serde(default)]
    pub require_at_least_one_filter: bool,

    /// Hard cap on returned rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rows: Option<u64>,

    /// Cap on HTTP pagination calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_pages: Option<u32>,

    /// Per-query timeout override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "humantime_serde::option")]
    #[schemars(with = "Option<String>")]
    pub timeout: Option<Duration>,

    /// Predicates AND-ed into every scan of the owning semantic model. May
    /// reference `${param:NAME}` placeholders, bound from
    /// `SemanticQuery::params` as escaped SQL literals at compile time (never
    /// interpolated as SQL fragments — see `pawrly-semantic`). Used for
    /// row-level security and always-on filters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_predicates: Vec<String>,
}

impl SafetyPolicy {
    /// Minimum-restriction policy used when no explicit safety block was provided.
    #[must_use]
    pub fn permissive() -> Self {
        Self::default()
    }

    /// Sensible defaults for raw-HTTP tables: require `request_path`.
    #[must_use]
    pub fn raw_http() -> Self {
        Self {
            require_filters_on: vec!["request_path".into()],
            require_at_least_one_filter: true,
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_round_trip() {
        let yaml = r#"
require_filters_on: [website_url]
max_rows: 10000
timeout: 2m
"#;
        let s: SafetyPolicy = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(s.require_filters_on, vec!["website_url"]);
        assert_eq!(s.max_rows, Some(10_000));
        assert_eq!(s.timeout, Some(Duration::from_secs(120)));
    }
}
