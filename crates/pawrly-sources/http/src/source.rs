//! Shared `HttpSource` configuration shared between typed and raw tables.

use std::collections::BTreeMap;
use std::sync::Arc;

use arrow_schema::{DataType, Field, Schema, SchemaRef};
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};

/// Auth declaration. Supports bearer + api-key + basic.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthSpec {
    #[default]
    None,
    Bearer {
        token: String,
    },
    ApiKey {
        header: String,
        value: String,
    },
    Basic {
        username: String,
        password: String,
    },
}

/// Parameter declaration on a typed HTTP table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamSpec {
    /// Name as declared in YAML (also the SQL column name when `source: param`).
    pub name: String,
    /// Type as a string (e.g. `varchar`, `int`).
    #[serde(default = "default_type")]
    pub r#type: String,
    /// Whether the parameter is required (= must appear as a filter).
    #[serde(default)]
    pub required: bool,
    /// Optional default value if the user didn't supply one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

fn default_type() -> String {
    "varchar".into()
}

/// How to walk from one page of results to the next.
///
/// The variant is selected by a `type` tag in YAML/JSON, e.g.
/// `{ "type": "page", "param": "page" }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PaginationConfig {
    /// RFC 5988 `Link:` header carrying a `rel="next"` URL.
    LinkHeader,
    /// Opaque cursor pulled from the response body at `next_path` (a `$.a.b`
    /// JSONPath) and echoed back as the `param` query parameter. Stop when the
    /// cursor is absent or empty.
    Cursor {
        /// `$.a.b` path to the next cursor inside the response body.
        next_path: String,
        /// Query-param name to send the cursor as on the next request.
        param: String,
    },
    /// Page-number pagination: increment `param` starting at `start`. Stop when
    /// a page returns zero rows. Optionally also send a page-size parameter.
    Page {
        /// Query-param name carrying the page number.
        param: String,
        /// First page number (typically 1).
        #[serde(default = "default_page_start")]
        start: u32,
        /// Optional query-param name carrying the page size.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size_param: Option<String>,
        /// Optional page size value sent via `size_param`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<u32>,
    },
    /// Offset/limit pagination: increment `param` by `size` each page. Stop when
    /// a page returns fewer than `size` rows (or zero).
    Offset {
        /// Query-param name carrying the offset.
        param: String,
        /// Query-param name carrying the page size.
        size_param: String,
        /// Page size (also the offset increment).
        size: u32,
    },
}

fn default_page_start() -> u32 {
    1
}

/// Retry policy for transient HTTP failures (transport errors, 5xx, 429).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    /// Maximum number of retries after the initial attempt.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// Base backoff in milliseconds; doubles each attempt.
    #[serde(default = "default_base_backoff_ms")]
    pub base_backoff_ms: u64,
    /// Ceiling on a single backoff in milliseconds.
    #[serde(default = "default_max_backoff_ms")]
    pub max_backoff_ms: u64,
}

fn default_max_retries() -> u32 {
    3
}

fn default_base_backoff_ms() -> u64 {
    200
}

fn default_max_backoff_ms() -> u64 {
    5_000
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: default_max_retries(),
            base_backoff_ms: default_base_backoff_ms(),
            max_backoff_ms: default_max_backoff_ms(),
        }
    }
}

/// Rate-limit policy: a steady ceiling of requests per second.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    pub requests_per_second: u32,
}

/// Per-table declaration for an HTTP source.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HttpTableSpec {
    pub name: String,
    pub endpoint: String,
    #[serde(default = "default_method")]
    pub method: String,
    #[serde(default)]
    pub params: Vec<ParamSpec>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Response body shape (minimal).
    pub response: ResponseSpec,
    /// Optional pagination strategy; absent means single-page fetch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pagination: Option<PaginationConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

fn default_method() -> String {
    "GET".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseSpec {
    /// JSONPath to the row array. `$` means the response body itself is the array.
    #[serde(default = "default_response_path")]
    pub path: String,
    /// Declared columns. Each column has a name + Arrow type.
    pub schema: Vec<ResponseColumn>,
}

fn default_response_path() -> String {
    "$".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseColumn {
    pub name: String,
    pub r#type: String,
    /// Optional: pull from a JSONPath inside each row, or `param` to inject a request param.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// In-memory shared state for an HTTP source.
pub struct HttpSource {
    pub name: String,
    pub base_url: url::Url,
    pub auth: AuthSpec,
    pub headers: HeaderMap,
    pub client: reqwest::Client,
    /// Retry policy applied to every request issued by this source.
    pub retry: RetryConfig,
    /// Optional in-memory, direct (un-keyed) rate limiter shared across scans.
    pub limiter: Option<Arc<governor::DefaultDirectRateLimiter>>,
}

impl std::fmt::Debug for HttpSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpSource")
            .field("name", &self.name)
            .field("base_url", &self.base_url.as_str())
            .field("retry", &self.retry)
            .field("rate_limited", &self.limiter.is_some())
            .finish()
    }
}

impl HttpSource {
    /// Build a `reqwest::Client` configured with reasonable defaults.
    pub fn build_client() -> reqwest::Client {
        reqwest::Client::builder()
            .user_agent(format!("pawrly/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    }
}

/// Build an Arrow `SchemaRef` from the declared response schema.
pub fn schema_for(table: &HttpTableSpec) -> SchemaRef {
    let fields: Vec<Field> = table
        .response
        .schema
        .iter()
        .map(|c| Field::new(&c.name, parse_arrow_type(&c.r#type), true))
        .collect();
    Arc::new(Schema::new(fields))
}

/// Map a YAML-declared type string to an Arrow `DataType`.
pub fn parse_arrow_type(s: &str) -> DataType {
    match s.to_ascii_lowercase().as_str() {
        "bool" | "boolean" => DataType::Boolean,
        "int" | "int32" => DataType::Int32,
        "bigint" | "int64" | "long" => DataType::Int64,
        "float" | "float32" => DataType::Float32,
        "double" | "float64" => DataType::Float64,
        "varchar" | "string" | "text" => DataType::Utf8,
        _ => DataType::Utf8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_arrow_types() {
        assert_eq!(parse_arrow_type("varchar"), DataType::Utf8);
        assert_eq!(parse_arrow_type("bigint"), DataType::Int64);
        assert_eq!(parse_arrow_type("int"), DataType::Int32);
        assert_eq!(parse_arrow_type("bool"), DataType::Boolean);
    }
}
