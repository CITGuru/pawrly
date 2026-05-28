//! Bundled HTTP source specs (github / linear / stripe).
//!
//! Currently bundles only `github` with one table (`pulls`); more endpoints and
//! sources are not yet bundled.

use std::collections::BTreeMap;

use pawrly_core::SourceKind;

use crate::source::{HttpTableSpec, ParamSpec, ResponseColumn, ResponseSpec};

/// Bundled spec embedded in the binary.
#[derive(Debug, Clone)]
pub struct BundledSpec {
    pub base_url: String,
    pub tables: Vec<HttpTableSpec>,
    pub raw_table_default: bool,
    pub default_headers: BTreeMap<String, String>,
}

/// Look up the bundled spec for a given source kind.
#[must_use]
pub fn for_kind(kind: SourceKind) -> Option<BundledSpec> {
    match kind {
        SourceKind::Github => Some(github()),
        _ => None,
    }
}

fn github() -> BundledSpec {
    let mut headers = BTreeMap::new();
    headers.insert("Accept".into(), "application/vnd.github+json".into());
    headers.insert("X-GitHub-Api-Version".into(), "2022-11-28".into());

    BundledSpec {
        base_url: "https://api.github.com".into(),
        raw_table_default: true,
        default_headers: headers,
        tables: vec![HttpTableSpec {
            name: "pulls".into(),
            endpoint: "/repos/{owner}/{repo}/pulls".into(),
            method: "GET".into(),
            params: vec![
                ParamSpec {
                    name: "owner".into(),
                    r#type: "varchar".into(),
                    required: true,
                    default: None,
                },
                ParamSpec {
                    name: "repo".into(),
                    r#type: "varchar".into(),
                    required: true,
                    default: None,
                },
                ParamSpec {
                    name: "state".into(),
                    r#type: "varchar".into(),
                    required: false,
                    default: Some("open".into()),
                },
            ],
            headers: BTreeMap::new(),
            response: ResponseSpec {
                path: "$".into(),
                schema: vec![
                    ResponseColumn {
                        name: "number".into(),
                        r#type: "int".into(),
                        source: None,
                    },
                    ResponseColumn {
                        name: "title".into(),
                        r#type: "varchar".into(),
                        source: None,
                    },
                    ResponseColumn {
                        name: "state".into(),
                        r#type: "varchar".into(),
                        source: None,
                    },
                    ResponseColumn {
                        name: "html_url".into(),
                        r#type: "varchar".into(),
                        source: None,
                    },
                    ResponseColumn {
                        name: "owner".into(),
                        r#type: "varchar".into(),
                        source: Some("param".into()),
                    },
                    ResponseColumn {
                        name: "repo".into(),
                        r#type: "varchar".into(),
                        source: Some("param".into()),
                    },
                ],
            },
            description: Some("Pull requests for a GitHub repository.".into()),
        }],
    }
}
