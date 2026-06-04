//! Shared `HttpSource` configuration shared between typed and raw tables.

use std::collections::BTreeMap;
use std::sync::Arc;

use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};

/// Auth declaration, tagged by `type`: `header` (tokens / API keys in headers),
/// `basic` (HTTP Basic), `custom` (credentials in the query string), and
/// `oauth2` (client-credentials token fetch).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthSpec {
    #[default]
    None,
    /// One or more headers attached to every request — bearer tokens and API
    /// keys alike.
    Header {
        #[serde(default)]
        headers: Vec<AuthHeader>,
    },
    Basic {
        username: String,
        password: String,
    },
    /// Credentials carried outside headers. Currently query parameters appended
    /// to every request (`?api_key=…`); reserved for request signers later.
    Custom {
        #[serde(default)]
        query: Vec<QueryCredential>,
    },
    /// OAuth2 client-credentials grant. A token is fetched on first use and
    /// re-fetched on expiry, then sent as `Authorization: Bearer <token>`.
    Oauth2 {
        token_url: String,
        client_id: String,
        client_secret: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audience: Option<String>,
    },
}

/// One header in a `type: header` auth block. Provide exactly one of `bearer`
/// (sent as `Bearer <bearer>`) or `value` (sent verbatim).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthHeader {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

/// One query-string credential in a `type: custom` auth block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryCredential {
    pub name: String,
    pub value: String,
}

impl AuthSpec {
    /// A single bearer header on `Authorization` — the `config.token` shorthand.
    #[must_use]
    pub fn bearer(token: impl Into<String>) -> Self {
        AuthSpec::Header {
            headers: vec![AuthHeader {
                name: "Authorization".into(),
                bearer: Some(token.into()),
                value: None,
            }],
        }
    }
}

/// A cached OAuth2 access token with its expiry.
#[derive(Debug, Clone)]
pub struct CachedToken {
    pub token: String,
    pub expires_at: std::time::SystemTime,
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
    /// Comparison operators (besides `=`) that may push down, e.g. `[">=", "<="]`.
    /// Each must have an `emit` mapping. Empty means equality only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepts: Vec<String>,
    /// For non-equality operators, the query parameter to emit the value as,
    /// keyed by operator token (`">="` -> `"since"`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub emit: BTreeMap<String, String>,
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

/// Rate-limit policy: a steady ceiling of requests per second, plus optional
/// awareness of the API's own quota headers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    /// Steady token-bucket ceiling. `None`/zero disables the local throttle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_per_second: Option<u32>,
    /// Response header carrying remaining quota (e.g. `x-ratelimit-remaining`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_header: Option<String>,
    /// Response header carrying the reset time as an epoch-seconds timestamp
    /// (e.g. `x-ratelimit-reset`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_header: Option<String>,
    /// Statuses (besides `429`/`503`) also treated as rate-limit signals, e.g.
    /// GitHub's secondary-limit `403`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_statuses: Vec<u16>,
}

/// Runtime rate-limit state on an [`HttpSource`]: the local throttle plus the
/// parsed header-awareness config.
#[derive(Default)]
pub struct RateLimitPolicy {
    /// Optional in-memory, direct (un-keyed) rate limiter shared across scans.
    pub limiter: Option<Arc<governor::DefaultDirectRateLimiter>>,
    /// Header carrying remaining quota; `0` triggers a wait until `reset_header`.
    pub remaining_header: Option<String>,
    /// Header carrying the reset time (epoch seconds).
    pub reset_header: Option<String>,
    /// Statuses treated as rate-limit signals in addition to `429`/`503`.
    pub extra_statuses: Vec<u16>,
}

/// Request body for non-GET endpoints (POST/PUT, GraphQL).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestBody {
    /// How to encode the rendered body.
    #[serde(default)]
    pub kind: BodyKind,
    /// Body text with `{param}` placeholders filled from bound params/filters.
    pub template: String,
}

/// Encoding for a [`RequestBody`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BodyKind {
    /// `application/json`.
    #[default]
    Json,
    /// `application/x-www-form-urlencoded`.
    Form,
}

/// One conditional request shape, selected when all of `when_filters` are bound.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConditionalRequest {
    /// Filter/param names that must all be bound to select this request.
    pub when_filters: Vec<String>,
    /// Endpoint template for this case (may carry `{param}` placeholders).
    pub endpoint: String,
    /// HTTP method for this case.
    #[serde(default = "default_method")]
    pub method: String,
    /// Optional request body for this case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<RequestBody>,
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
    /// Optional request body (POST/PUT/GraphQL); absent means no body. This is
    /// the body of the *default* request (see `requests`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<RequestBody>,
    /// Conditional requests, tried in order: the first whose `when_filters` are
    /// all bound is used instead of the default `endpoint`/`method`/`body`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requests: Vec<ConditionalRequest>,
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
    /// Treat a `404` as an empty result set instead of an error.
    #[serde(default)]
    pub allow_404_empty: bool,
    /// Optional error detection: turn API failures into a clear scan error
    /// instead of silently parsing them as rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseErrorSpec>,
}

fn default_response_path() -> String {
    "$".into()
}

/// Declares how to recognize an error response and what message to surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseErrorSpec {
    /// Status codes (or matchers like `">=400"`, `"5xx"`) that fail the scan.
    #[serde(default)]
    pub status: Vec<StatusMatcher>,
    /// JSONPath to an error message inside a `200`-with-error body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// A single status-code condition: an exact code (`403`) or an expression
/// (`">=400"`, `"5xx"`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StatusMatcher {
    Exact(u16),
    Expr(String),
}

impl StatusMatcher {
    /// Whether `status` satisfies this matcher.
    pub fn matches(&self, status: u16) -> bool {
        match self {
            StatusMatcher::Exact(code) => *code == status,
            StatusMatcher::Expr(expr) => match_status_expr(expr, status),
        }
    }
}

/// Evaluate a status expression: `">=400"`, `"<500"`, `"5xx"`, or a bare code.
fn match_status_expr(expr: &str, status: u16) -> bool {
    let expr = expr.trim();
    if let Some(prefix) = expr.strip_suffix("xx") {
        // `"5xx"` matches 500..=599.
        if let Ok(hundreds) = prefix.parse::<u16>() {
            let base = hundreds * 100;
            return status >= base && status < base + 100;
        }
        return false;
    }
    for op in [">=", "<=", ">", "<", "==", "="] {
        if let Some(rest) = expr.strip_prefix(op) {
            let Ok(code) = rest.trim().parse::<u16>() else {
                return false;
            };
            return match op {
                ">=" => status >= code,
                "<=" => status <= code,
                ">" => status > code,
                "<" => status < code,
                _ => status == code,
            };
        }
    }
    expr.parse::<u16>().map(|c| c == status).unwrap_or(false)
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
    /// Rate-limit policy (local throttle + header awareness).
    pub rate_limit: RateLimitPolicy,
    /// Cached OAuth2 access token, populated on first use (when `auth` is OAuth2).
    pub oauth_token: tokio::sync::Mutex<Option<CachedToken>>,
}

impl std::fmt::Debug for HttpSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpSource")
            .field("name", &self.name)
            .field("base_url", &self.base_url.as_str())
            .field("retry", &self.retry)
            .field("rate_limited", &self.rate_limit.limiter.is_some())
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

    /// Apply this source's auth to a request, fetching/refreshing an OAuth2
    /// token when needed.
    pub async fn apply_auth(
        &self,
        req: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, String> {
        Ok(match &self.auth {
            AuthSpec::None => req,
            AuthSpec::Header { headers } => {
                let mut req = req;
                for h in headers {
                    match (&h.bearer, &h.value) {
                        (Some(b), _) => req = req.header(&h.name, format!("Bearer {b}")),
                        (None, Some(v)) => req = req.header(&h.name, v),
                        (None, None) => {}
                    }
                }
                req
            }
            AuthSpec::Basic { username, password } => req.basic_auth(username, Some(password)),
            AuthSpec::Custom { query } => {
                let pairs: Vec<(&str, &str)> = query
                    .iter()
                    .map(|q| (q.name.as_str(), q.value.as_str()))
                    .collect();
                req.query(&pairs)
            }
            AuthSpec::Oauth2 { .. } => req.bearer_auth(self.oauth_bearer().await?),
        })
    }

    /// Return a valid OAuth2 access token, reusing the cached one until it is
    /// within 30s of expiry, otherwise performing a client-credentials exchange.
    async fn oauth_bearer(&self) -> Result<String, String> {
        let AuthSpec::Oauth2 {
            token_url,
            client_id,
            client_secret,
            scope,
            audience,
        } = &self.auth
        else {
            return Err("oauth_bearer called for non-OAuth2 source".into());
        };

        let mut guard = self.oauth_token.lock().await;
        let soon = std::time::SystemTime::now() + std::time::Duration::from_secs(30);
        if let Some(cached) = guard.as_ref()
            && cached.expires_at > soon
        {
            return Ok(cached.token.clone());
        }

        let mut form = vec![
            ("grant_type", "client_credentials"),
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
        ];
        if let Some(s) = scope {
            form.push(("scope", s.as_str()));
        }
        if let Some(a) = audience {
            form.push(("audience", a.as_str()));
        }

        let resp = self
            .client
            .post(token_url)
            .form(&form)
            .send()
            .await
            .map_err(|e| format!("oauth token request failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("oauth token request returned {}", resp.status()));
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("oauth token response was not JSON: {e}"))?;
        let token = body
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or("oauth token response missing `access_token`")?
            .to_string();
        // Default to a short-but-safe lifetime when `expires_in` is absent.
        let expires_in = body
            .get("expires_in")
            .and_then(|v| v.as_u64())
            .unwrap_or(300);
        let expires_at = std::time::SystemTime::now() + std::time::Duration::from_secs(expires_in);

        *guard = Some(CachedToken {
            token: token.clone(),
            expires_at,
        });
        Ok(token)
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
///
/// `json` keeps a nested object/array as raw JSON text (Arrow `Utf8`); the
/// distinction from `varchar` lives on [`ResponseColumn::r#type`], which the row
/// builder consults to decide whether to JSON-encode the value.
pub fn parse_arrow_type(s: &str) -> DataType {
    match s.to_ascii_lowercase().as_str() {
        "bool" | "boolean" => DataType::Boolean,
        "int" | "int32" => DataType::Int32,
        "bigint" | "int64" | "long" => DataType::Int64,
        "float" | "float32" => DataType::Float32,
        "double" | "float64" => DataType::Float64,
        "date" => DataType::Date32,
        "timestamp" => DataType::Timestamp(TimeUnit::Microsecond, None),
        "timestamptz" => DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
        "varchar" | "string" | "text" | "json" => DataType::Utf8,
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
        assert_eq!(parse_arrow_type("date"), DataType::Date32);
        assert_eq!(
            parse_arrow_type("timestamp"),
            DataType::Timestamp(TimeUnit::Microsecond, None)
        );
        assert_eq!(
            parse_arrow_type("timestamptz"),
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into()))
        );
        // `json` is stored as raw text.
        assert_eq!(parse_arrow_type("json"), DataType::Utf8);
    }

    #[test]
    fn status_matchers() {
        assert!(StatusMatcher::Exact(403).matches(403));
        assert!(!StatusMatcher::Exact(403).matches(404));
        assert!(StatusMatcher::Expr(">=400".into()).matches(404));
        assert!(!StatusMatcher::Expr(">=400".into()).matches(200));
        assert!(StatusMatcher::Expr("<500".into()).matches(404));
        assert!(StatusMatcher::Expr("5xx".into()).matches(503));
        assert!(!StatusMatcher::Expr("5xx".into()).matches(404));
        assert!(StatusMatcher::Expr("418".into()).matches(418));
    }
}
