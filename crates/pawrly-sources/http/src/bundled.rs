//! Bundled HTTP source specs.
//!
//! Each `kind:` that ships a curated spec maps to one `BundledSpec` embedded in
//! the binary; the user supplies only credentials (`config.token`) and optional
//! overrides (`config.base_url`). The table shapes map each provider's API onto
//! pawrly's HTTP table model (GET endpoints, `$.path` extraction, bearer auth,
//! and the four pagination strategies).
//!
//! Bundled today: `github` (pulls / issues), `sentry` (projects / issues),
//! `slack` (users / channels). Other kinds fall back to user-declared tables.

use std::collections::BTreeMap;

use pawrly_core::SourceKind;

use crate::source::{HttpTableSpec, PaginationConfig, ParamSpec, ResponseColumn, ResponseSpec};

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
        SourceKind::Sentry => Some(sentry()),
        SourceKind::Slack => Some(slack()),
        _ => None,
    }
}

// ---- small builders to keep the spec tables readable ----------------------

/// A column read from the row's top-level field of the same name.
fn col(name: &str, ty: &str) -> ResponseColumn {
    ResponseColumn {
        name: name.into(),
        r#type: ty.into(),
        source: None,
    }
}

/// A column read from a `$.a.b` path inside each row (or `param` to inject a
/// request parameter as a column).
fn col_src(name: &str, ty: &str, source: &str) -> ResponseColumn {
    ResponseColumn {
        name: name.into(),
        r#type: ty.into(),
        source: Some(source.into()),
    }
}

fn required_param(name: &str) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        r#type: "varchar".into(),
        required: true,
        default: None,
        accepts: Vec::new(),
        emit: BTreeMap::new(),
    }
}

fn optional_param(name: &str, default: Option<&str>) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        r#type: "varchar".into(),
        required: false,
        default: default.map(str::to_string),
        accepts: Vec::new(),
        emit: BTreeMap::new(),
    }
}

// ---- github ----------------------------------------------------------------

fn github() -> BundledSpec {
    let mut headers = BTreeMap::new();
    headers.insert("Accept".into(), "application/vnd.github+json".into());
    headers.insert("X-GitHub-Api-Version".into(), "2022-11-28".into());

    // owner + repo are required path params on both tables; state defaults to
    // open. GitHub paginates with RFC 5988 `Link` headers.
    let owner_repo_state = vec![
        required_param("owner"),
        required_param("repo"),
        optional_param("state", Some("open")),
    ];

    BundledSpec {
        base_url: "https://api.github.com".into(),
        raw_table_default: true,
        default_headers: headers,
        tables: vec![
            HttpTableSpec {
                name: "pulls".into(),
                endpoint: "/repos/{owner}/{repo}/pulls".into(),
                method: "GET".into(),
                params: owner_repo_state.clone(),
                headers: BTreeMap::new(),
                body: None,
                response: ResponseSpec {
                    path: "$".into(),
                    schema: vec![
                        col("number", "int"),
                        col("title", "varchar"),
                        col("state", "varchar"),
                        col("html_url", "varchar"),
                        col_src("owner", "varchar", "param"),
                        col_src("repo", "varchar", "param"),
                    ],
                    allow_404_empty: false,
                    error: None,
                },
                pagination: Some(PaginationConfig::LinkHeader),
                description: Some("Pull requests for a GitHub repository.".into()),
            },
            HttpTableSpec {
                name: "issues".into(),
                endpoint: "/repos/{owner}/{repo}/issues".into(),
                method: "GET".into(),
                params: owner_repo_state,
                headers: BTreeMap::new(),
                body: None,
                response: ResponseSpec {
                    path: "$".into(),
                    schema: vec![
                        col("number", "int"),
                        col("title", "varchar"),
                        col("state", "varchar"),
                        col("html_url", "varchar"),
                        col_src("user", "varchar", "$.user.login"),
                        col("comments", "bigint"),
                        col_src("owner", "varchar", "param"),
                        col_src("repo", "varchar", "param"),
                    ],
                    allow_404_empty: false,
                    error: None,
                },
                pagination: Some(PaginationConfig::LinkHeader),
                description: Some(
                    "Issues for a GitHub repository (includes pull requests).".into(),
                ),
            },
        ],
    }
}

// ---- sentry ----------------------------------------------------------------

fn sentry() -> BundledSpec {
    // Sentry: bearer token; org is a path param; `Link` headers carry
    // `results="true|false"` to drive pagination. Field names are camelCase, so
    // most columns read from an explicit `$.camelCase` path.
    BundledSpec {
        base_url: "https://sentry.io".into(),
        raw_table_default: false,
        default_headers: BTreeMap::new(),
        tables: vec![
            HttpTableSpec {
                name: "projects".into(),
                endpoint: "/api/0/organizations/{org}/projects/".into(),
                method: "GET".into(),
                params: vec![required_param("org")],
                headers: BTreeMap::new(),
                body: None,
                response: ResponseSpec {
                    path: "$".into(),
                    schema: vec![
                        col("id", "varchar"),
                        col("slug", "varchar"),
                        col("name", "varchar"),
                        col("platform", "varchar"),
                        col("status", "varchar"),
                        col_src("date_created", "varchar", "$.dateCreated"),
                        col_src("org", "varchar", "param"),
                    ],
                    allow_404_empty: false,
                    error: None,
                },
                pagination: Some(PaginationConfig::LinkHeader),
                description: Some("Projects in a Sentry organization.".into()),
            },
            HttpTableSpec {
                name: "issues".into(),
                endpoint: "/api/0/organizations/{org}/issues/".into(),
                method: "GET".into(),
                params: vec![required_param("org")],
                headers: BTreeMap::new(),
                body: None,
                response: ResponseSpec {
                    path: "$".into(),
                    schema: vec![
                        col("id", "varchar"),
                        col_src("short_id", "varchar", "$.shortId"),
                        col("title", "varchar"),
                        col("status", "varchar"),
                        col("level", "varchar"),
                        col("count", "bigint"),
                        col_src("user_count", "bigint", "$.userCount"),
                        col_src("first_seen", "varchar", "$.firstSeen"),
                        col_src("last_seen", "varchar", "$.lastSeen"),
                        col_src("project", "varchar", "$.project.slug"),
                        col_src("org", "varchar", "param"),
                    ],
                    allow_404_empty: false,
                    error: None,
                },
                pagination: Some(PaginationConfig::LinkHeader),
                description: Some("Unresolved issues in a Sentry organization.".into()),
            },
        ],
    }
}

// ---- slack -----------------------------------------------------------------

fn slack() -> BundledSpec {
    // Slack Web API: bearer token; cursor pagination via
    // `response_metadata.next_cursor` echoed back as `?cursor=`. Rows live under
    // `members` / `channels`.
    let cursor = PaginationConfig::Cursor {
        next_path: "$.response_metadata.next_cursor".into(),
        param: "cursor".into(),
    };
    BundledSpec {
        base_url: "https://slack.com".into(),
        raw_table_default: false,
        default_headers: BTreeMap::new(),
        tables: vec![
            HttpTableSpec {
                name: "users".into(),
                endpoint: "/api/users.list".into(),
                method: "GET".into(),
                params: Vec::new(),
                headers: BTreeMap::new(),
                body: None,
                response: ResponseSpec {
                    path: "$.members".into(),
                    schema: vec![
                        col("id", "varchar"),
                        col("name", "varchar"),
                        col("real_name", "varchar"),
                        col_src("display_name", "varchar", "$.profile.display_name"),
                        col_src("email", "varchar", "$.profile.email"),
                        col("is_bot", "bool"),
                        col("is_admin", "bool"),
                        col("deleted", "bool"),
                    ],
                    allow_404_empty: false,
                    error: None,
                },
                pagination: Some(cursor.clone()),
                description: Some("Members of a Slack workspace (users.list).".into()),
            },
            HttpTableSpec {
                name: "channels".into(),
                endpoint: "/api/conversations.list".into(),
                method: "GET".into(),
                params: vec![optional_param(
                    "types",
                    Some("public_channel,private_channel"),
                )],
                headers: BTreeMap::new(),
                body: None,
                response: ResponseSpec {
                    path: "$.channels".into(),
                    schema: vec![
                        col("id", "varchar"),
                        col("name", "varchar"),
                        col_src("topic", "varchar", "$.topic.value"),
                        col_src("purpose", "varchar", "$.purpose.value"),
                        col("num_members", "bigint"),
                        col("is_archived", "bool"),
                        col("created", "bigint"),
                    ],
                    allow_404_empty: false,
                    error: None,
                },
                pagination: Some(cursor),
                description: Some("Channels in a Slack workspace (conversations.list).".into()),
            },
        ],
    }
}
