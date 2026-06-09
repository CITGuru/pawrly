//! Typed HTTP table provider: one declared endpoint with declared columns.
//!
//! Simplifications:
//! - Filter pushdown is done by lifting `WHERE col = literal` filters that
//!   match a declared parameter and substituting them into the URL path /
//!   query string.
//! - Pagination follows the table's `PaginationConfig` (link header, cursor,
//!   page, or offset); absent config means a single-page fetch.
//! - Required params must appear as `WHERE col = value` filters; otherwise
//!   the scan returns a clear error.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use arrow_array::{
    ArrayRef, BooleanArray, RecordBatch,
    builder::{
        Date32Builder, Float64Builder, Int32Builder, Int64Builder, StringBuilder,
        TimestampMicrosecondBuilder,
    },
};
use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};
use async_trait::async_trait;
use datafusion::catalog::Session;
use datafusion::common::DataFusionError;
use datafusion::datasource::TableProvider;
use datafusion::datasource::TableType;
use datafusion::datasource::memory::MemorySourceConfig;
use datafusion::logical_expr::{Expr, TableProviderFilterPushDown};
use datafusion::physical_plan::ExecutionPlan;
use serde_json::Value;

use crate::paginate::{self, NextPage};
use crate::source::{
    BodyKind, HttpSource, HttpTableSpec, RateLimitPolicy, RequestBody, ResponseColumn,
    custom_body_object, schema_for,
};

#[derive(Debug)]
pub struct HttpTableProvider {
    pub source: Arc<HttpSource>,
    pub spec: Arc<HttpTableSpec>,
    pub schema: SchemaRef,
    /// Hard cap on pagination calls, threaded from the table/source safety
    /// policy. `None` means no cap.
    pub max_pages: Option<u32>,
}

impl pawrly_core::DynamicFilterCapable for HttpTableProvider {
    fn dynamic_filter_columns(&self) -> Vec<String> {
        // Declared params can absorb runtime `IN(...)` filters on equality.
        self.spec.params.iter().map(|p| p.name.clone()).collect()
    }
}

impl HttpTableProvider {
    pub fn new(source: Arc<HttpSource>, spec: Arc<HttpTableSpec>) -> Self {
        Self::with_max_pages(source, spec, None)
    }

    pub fn with_max_pages(
        source: Arc<HttpSource>,
        spec: Arc<HttpTableSpec>,
        max_pages: Option<u32>,
    ) -> Self {
        let schema = schema_for(&spec);
        Self {
            source,
            spec,
            schema,
            max_pages,
        }
    }

    /// Whether a filter can be pushed into the request: an equality on a
    /// declared param, or a comparison the param's `accepts`/`emit` covers.
    fn can_push_down(&self, expr: &Expr) -> bool {
        let Some((col, op, _)) = extract_cmp(expr) else {
            return false;
        };
        let Some(param) = self.spec.params.iter().find(|p| p.name == col) else {
            return false;
        };
        op == "=" || (param.accepts.iter().any(|a| a == op) && param.emit.contains_key(op))
    }

    /// Select the request shape (endpoint, method, body) for this scan: the
    /// first conditional request whose `when_filters` are all bound, else the
    /// table's default endpoint/method/body.
    fn select_request(
        &self,
        params: &BTreeMap<String, String>,
    ) -> (&str, &str, Option<&RequestBody>) {
        for r in &self.spec.requests {
            if r.when_filters.iter().all(|f| params.contains_key(f)) {
                return (r.endpoint.as_str(), r.method.as_str(), r.body.as_ref());
            }
        }
        (
            self.spec.endpoint.as_str(),
            self.spec.method.as_str(),
            self.spec.body.as_ref(),
        )
    }

    /// Whether a `Content-Type` is already pinned by the source or table headers
    /// (so the body builder shouldn't add its own).
    fn has_content_type(&self) -> bool {
        self.source
            .headers
            .keys()
            .any(|k| k.as_str().eq_ignore_ascii_case("content-type"))
            || self
                .spec
                .headers
                .keys()
                .any(|k| k.eq_ignore_ascii_case("content-type"))
    }
}

#[async_trait]
impl TableProvider for HttpTableProvider {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> datafusion::common::Result<Vec<TableProviderFilterPushDown>> {
        Ok(filters
            .iter()
            .map(|f| {
                if self.can_push_down(f) {
                    TableProviderFilterPushDown::Exact
                } else {
                    TableProviderFilterPushDown::Unsupported
                }
            })
            .collect())
    }

    async fn scan(
        &self,
        state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
    ) -> datafusion::common::Result<Arc<dyn ExecutionPlan>> {
        let _ = state;
        let mut params: BTreeMap<String, String> = BTreeMap::new();
        for f in filters {
            let Some((col, op, val)) = extract_cmp(f) else {
                continue;
            };
            let Some(param) = self.spec.params.iter().find(|p| p.name == col) else {
                continue;
            };
            if op == "=" {
                params.insert(col, val);
            } else if let Some(query_param) = param.emit.get(op) {
                // A comparison maps to the emit-declared query parameter.
                params.insert(query_param.clone(), val);
            }
        }
        // Defaults
        for p in &self.spec.params {
            if let Some(default) = &p.default
                && !params.contains_key(&p.name)
            {
                params.insert(p.name.clone(), default.clone());
            }
        }
        // Required
        for p in &self.spec.params {
            if p.required && !params.contains_key(&p.name) {
                return Err(DataFusionError::Plan(format!(
                    "table `{}` requires filter `{} = ...` (PAWRLY_SAFETY_REQUIRED_FILTER)",
                    self.spec.name, p.name
                )));
            }
        }

        // Pick the request shape (endpoint/method/body) for this scan.
        let (endpoint, method_str, body) = self.select_request(&params);
        let method = method_str.parse().unwrap_or(reqwest::Method::GET);

        // Paginated fetch loop. `next_params`/`next_url` carry the target for the
        // upcoming request (initially the table endpoint + params). Each
        // iteration sends one page, accumulates rows, and consults `paginate`
        // for the next.
        let mut all_rows: Vec<Value> = Vec::new();
        let mut next_params = params.clone();
        if let Some(config) = &self.spec.pagination {
            paginate::seed_initial(config, &mut next_params);
        }
        let mut next_url: Option<url::Url> = None;
        let mut page_index: usize = 0;
        let mut throttle_until: Option<std::time::SystemTime> = None;

        loop {
            // Enforce the page cap before issuing the request for this page.
            if let Some(max) = self.max_pages
                && page_index as u64 >= max as u64
            {
                let err = pawrly_core::SafetyError::TooManyPages {
                    table: self.spec.name.clone(),
                    max_pages: max,
                };
                return Err(DataFusionError::External(Box::new(std::io::Error::other(
                    err.to_string(),
                ))));
            }

            // Honor the API's own quota headers reported by the previous page.
            if let Some(when) = throttle_until.take()
                && let Ok(dur) = when.duration_since(std::time::SystemTime::now())
            {
                tokio::time::sleep(dur).await;
            }

            // Rate limit: wait for a permit before each request.
            if let Some(limiter) = &self.source.rate_limit.limiter {
                limiter.until_ready().await;
            }

            let url = match &next_url {
                Some(u) => u.clone(),
                None => build_url(&self.source.base_url, endpoint, &next_params)?,
            };

            let mut req = self.source.client.request(method.clone(), url);
            for (k, v) in &self.source.headers {
                req = req.header(k, v);
            }
            for (k, v) in &self.spec.headers {
                req = req.header(k, v);
            }
            req = self
                .source
                .apply_auth(req)
                .await
                .map_err(|e| DataFusionError::External(Box::new(std::io::Error::other(e))))?;

            // Render and attach a request body, merging in any `custom` auth body
            // fields (a JSON object): merged on top of a JSON table body, or sent
            // as the whole body when the table declares none.
            let auth_body = self.source.custom_body_fields();
            match (body, auth_body.is_empty()) {
                // Table body only — render and attach as declared.
                (Some(tbody), true) => {
                    let rendered = render_template(&tbody.template, &next_params);
                    if !self.has_content_type() {
                        let content_type = match tbody.kind {
                            BodyKind::Json => "application/json",
                            BodyKind::Form => "application/x-www-form-urlencoded",
                        };
                        req = req.header(reqwest::header::CONTENT_TYPE, content_type);
                    }
                    req = req.body(rendered);
                }
                // Table body + auth body — merge the auth fields into the JSON body.
                (Some(tbody), false) => {
                    if tbody.kind != BodyKind::Json {
                        return Err(DataFusionError::Plan(
                            "custom auth `body` requires a JSON table body to merge into".into(),
                        ));
                    }
                    let rendered = render_template(&tbody.template, &next_params);
                    let mut value: Value = serde_json::from_str(&rendered).map_err(|e| {
                        DataFusionError::Plan(format!("table body is not valid JSON: {e}"))
                    })?;
                    let obj = value.as_object_mut().ok_or_else(|| {
                        DataFusionError::Plan(
                            "table body must be a JSON object to merge custom auth body".into(),
                        )
                    })?;
                    obj.extend(custom_body_object(auth_body));
                    if !self.has_content_type() {
                        req = req.header(reqwest::header::CONTENT_TYPE, "application/json");
                    }
                    req = req.body(value.to_string());
                }
                // Auth body only — send it as the whole JSON body.
                (None, false) => {
                    if !self.has_content_type() {
                        req = req.header(reqwest::header::CONTENT_TYPE, "application/json");
                    }
                    req = req.body(Value::Object(custom_body_object(auth_body)).to_string());
                }
                (None, true) => {}
            }

            let resp = send_with_retry(req, &self.source.retry, &self.source.rate_limit).await?;
            let status = resp.status();
            let status_code = status.as_u16();
            let headers = resp.headers().clone();

            // If the API reports its quota exhausted, defer the next request.
            throttle_until = compute_throttle(&self.source.rate_limit, &headers);

            // A 404 can be a legitimate "empty collection" rather than a failure.
            if status_code == 404 && self.spec.response.allow_404_empty {
                break;
            }

            let text = resp.text().await.map_err(|e| {
                DataFusionError::External(Box::new(std::io::Error::other(format!(
                    "reading response body failed (status {status}): {e}"
                ))))
            })?;
            let body: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    // A non-JSON body is only an error if we can't otherwise
                    // explain it via the declared error spec.
                    if let Some(spec) = &self.spec.response.error
                        && let Some(msg) = detect_error(spec, status_code, &Value::Null)
                    {
                        return Err(scan_error(msg));
                    }
                    return Err(DataFusionError::External(Box::new(std::io::Error::other(
                        format!("json parse failed (status {status}): {e}"),
                    ))));
                }
            };

            // Explicit error detection, when declared.
            if let Some(spec) = &self.spec.response.error
                && let Some(msg) = detect_error(spec, status_code, &body)
            {
                return Err(scan_error(msg));
            }

            let rows = extract_rows(&body, &self.spec.response.path)?;
            let row_count = rows.len();
            all_rows.extend(rows);

            // Stop early once enough rows are collected to satisfy a LIMIT.
            if let Some(lim) = limit
                && all_rows.len() >= lim
            {
                break;
            }

            // Without a pagination config we fetch exactly one page.
            let Some(config) = &self.spec.pagination else {
                break;
            };

            match paginate::next_page(config, &next_params, &body, &headers, row_count, page_index)
            {
                Some(NextPage::Params(p)) => {
                    next_params = p;
                    next_url = None;
                }
                Some(NextPage::Url(u)) => {
                    let parsed = url::Url::parse(&u).map_err(|e| {
                        DataFusionError::Plan(format!("bad pagination url `{u}`: {e}"))
                    })?;
                    next_url = Some(parsed);
                }
                None => break,
            }
            page_index += 1;
        }

        if let Some(lim) = limit {
            all_rows.truncate(lim);
        }

        let batch = build_batch(&self.schema, &self.spec.response.schema, &all_rows, &params)?;
        let projected_schema = if let Some(p) = projection {
            let fields: Vec<Field> = p.iter().map(|i| self.schema.field(*i).clone()).collect();
            Arc::new(Schema::new(fields))
        } else {
            self.schema.clone()
        };
        let projected: RecordBatch = if let Some(p) = projection {
            let cols: Vec<ArrayRef> = p.iter().map(|i| batch.column(*i).clone()).collect();
            RecordBatch::try_new(projected_schema.clone(), cols)
                .map_err(|e| DataFusionError::ArrowError(Box::new(e), None))?
        } else {
            batch
        };

        let exec = MemorySourceConfig::try_new_exec(&[vec![projected]], projected_schema, None)
            .map_err(|e| DataFusionError::Plan(e.to_string()))?;
        Ok(exec)
    }
}

/// Send a request with retry/backoff on transient failures.
///
/// Retries on transport errors and HTTP 5xx with exponential backoff
/// (`base * 2^attempt`, capped at `max_backoff`). For 429/503 and any
/// `rate_limit.extra_statuses` (e.g. GitHub's `403`), prefers the reset header
/// (epoch seconds) then a numeric `Retry-After` (seconds), otherwise falls back
/// to the backoff. After exhausting `max_retries`, returns the last error
/// wrapped as a `DataFusionError::External`.
async fn send_with_retry(
    req: reqwest::RequestBuilder,
    retry: &crate::source::RetryConfig,
    rate_limit: &RateLimitPolicy,
) -> datafusion::common::Result<reqwest::Response> {
    use reqwest::StatusCode;

    let mut attempt: u32 = 0;
    loop {
        // Clone so we can re-issue on retry; if the body isn't cloneable we
        // simply can't retry and send the original.
        let send_target = match req.try_clone() {
            Some(c) => c,
            None => {
                return req.send().await.map_err(|e| {
                    DataFusionError::External(Box::new(std::io::Error::other(format!(
                        "http request failed: {e}"
                    ))))
                });
            }
        };

        match send_target.send().await {
            Ok(resp) => {
                let status = resp.status();
                let is_rate_limit = status == StatusCode::TOO_MANY_REQUESTS
                    || status == StatusCode::SERVICE_UNAVAILABLE
                    || rate_limit.extra_statuses.contains(&status.as_u16());
                let retryable = status.is_server_error() || is_rate_limit;
                if !retryable || attempt >= retry.max_retries {
                    return Ok(resp);
                }
                // For rate-limit signals, prefer the reset header, then
                // Retry-After, before falling back to exponential backoff.
                let wait_ms = if is_rate_limit {
                    reset_wait_ms(rate_limit, &resp)
                        .or_else(|| retry_after_ms(&resp))
                        .unwrap_or_else(|| backoff_ms(retry, attempt))
                } else {
                    backoff_ms(retry, attempt)
                };
                tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
            }
            Err(e) => {
                if attempt >= retry.max_retries {
                    return Err(DataFusionError::External(Box::new(std::io::Error::other(
                        format!("http request failed: {e}"),
                    ))));
                }
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms(retry, attempt)))
                    .await;
            }
        }
        attempt += 1;
    }
}

/// Exponential backoff for `attempt` (0-based), capped at `max_backoff_ms`.
fn backoff_ms(retry: &crate::source::RetryConfig, attempt: u32) -> u64 {
    let factor = 1u64.checked_shl(attempt).unwrap_or(u64::MAX);
    retry
        .base_backoff_ms
        .saturating_mul(factor)
        .min(retry.max_backoff_ms)
}

/// Parse a numeric `Retry-After` header (delay in seconds) into milliseconds.
/// HTTP-date forms are ignored (we fall back to backoff).
fn retry_after_ms(resp: &reqwest::Response) -> Option<u64> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(|secs| secs.saturating_mul(1000))
}

/// Milliseconds to wait until the policy's reset header (epoch seconds), if it
/// is present and still in the future.
fn reset_wait_ms(policy: &RateLimitPolicy, resp: &reqwest::Response) -> Option<u64> {
    let target = reset_at(policy, resp.headers())?;
    let dur = target.duration_since(std::time::SystemTime::now()).ok()?;
    u64::try_from(dur.as_millis()).ok()
}

/// When the API reports `remaining == 0`, the time to defer the next request to
/// (the reset header). Returns `None` when not throttled or no reset is known.
fn compute_throttle(
    policy: &RateLimitPolicy,
    headers: &reqwest::header::HeaderMap,
) -> Option<std::time::SystemTime> {
    let remaining_header = policy.remaining_header.as_deref()?;
    let remaining: i64 = headers
        .get(remaining_header)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    if remaining > 0 {
        return None;
    }
    reset_at(policy, headers)
}

/// Resolve the policy's reset header into an absolute time (epoch seconds).
fn reset_at(
    policy: &RateLimitPolicy,
    headers: &reqwest::header::HeaderMap,
) -> Option<std::time::SystemTime> {
    let reset_header = policy.reset_header.as_deref()?;
    let reset_epoch: u64 = headers
        .get(reset_header)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(reset_epoch))
}

/// Render a body/template, substituting declared `{param}` placeholders with
/// bound values. Other braces (e.g. JSON/GraphQL) are left untouched.
fn render_template(template: &str, params: &BTreeMap<String, String>) -> String {
    let mut out = template.to_string();
    for (k, v) in params {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// Extract `column <op> literal` (or the flipped `literal <op> column`) where
/// `op` is a comparison. Returns the column name, a canonical operator token,
/// and the literal as a string. The token always reads "column op value", so a
/// flipped `literal > column` is normalized to `<`.
fn extract_cmp(expr: &Expr) -> Option<(String, &'static str, String)> {
    use datafusion::logical_expr::{BinaryExpr, Operator};
    let Expr::BinaryExpr(BinaryExpr { left, op, right }) = expr else {
        return None;
    };
    let (col, scalar, flipped) = match (left.as_ref(), right.as_ref()) {
        (Expr::Column(c), Expr::Literal(s, _)) => (c, s, false),
        (Expr::Literal(s, _), Expr::Column(c)) => (c, s, true),
        _ => return None,
    };
    let token = match (op, flipped) {
        (Operator::Eq, _) => "=",
        (Operator::Gt, false) | (Operator::Lt, true) => ">",
        (Operator::Lt, false) | (Operator::Gt, true) => "<",
        (Operator::GtEq, false) | (Operator::LtEq, true) => ">=",
        (Operator::LtEq, false) | (Operator::GtEq, true) => "<=",
        _ => return None,
    };
    Some((col.name.clone(), token, scalar_to_string(scalar)?))
}

fn scalar_to_string(scalar: &datafusion::scalar::ScalarValue) -> Option<String> {
    use datafusion::scalar::ScalarValue;
    match scalar {
        ScalarValue::Utf8(Some(s)) | ScalarValue::LargeUtf8(Some(s)) => Some(s.clone()),
        ScalarValue::Int32(Some(n)) => Some(n.to_string()),
        ScalarValue::Int64(Some(n)) => Some(n.to_string()),
        ScalarValue::UInt64(Some(n)) => Some(n.to_string()),
        ScalarValue::Boolean(Some(b)) => Some(b.to_string()),
        _ => None,
    }
}

fn build_url(
    base: &url::Url,
    endpoint: &str,
    params: &BTreeMap<String, String>,
) -> datafusion::common::Result<url::Url> {
    let mut path = endpoint.to_string();
    let mut query_params: Vec<(String, String)> = Vec::new();
    for (k, v) in params {
        let needle = format!("{{{k}}}");
        if path.contains(&needle) {
            path = path.replace(&needle, v);
        } else {
            query_params.push((k.clone(), v.clone()));
        }
    }
    let trimmed = path.trim_start_matches('/');
    let mut url = base
        .join(trimmed)
        .map_err(|e| DataFusionError::Plan(format!("bad url: {e}")))?;
    if !query_params.is_empty() {
        let mut q = url.query_pairs_mut();
        for (k, v) in &query_params {
            q.append_pair(k, v);
        }
    }
    Ok(url)
}

/// Wrap an error message as a `DataFusionError` that fails the scan.
fn scan_error(msg: String) -> DataFusionError {
    DataFusionError::External(Box::new(std::io::Error::other(msg)))
}

/// Decide whether a response is an error per the declared [`ResponseErrorSpec`],
/// returning the message to surface. A status hit or a non-null value at
/// `error.path` triggers; the path value (when present) is the message.
fn detect_error(
    spec: &crate::source::ResponseErrorSpec,
    status: u16,
    body: &Value,
) -> Option<String> {
    let status_hit = spec.status.iter().any(|m| m.matches(status));
    let path_msg = spec
        .path
        .as_deref()
        .and_then(|p| match paginate::json_at_path(body, p)? {
            Value::Null => None,
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            Value::String(_) => None,
            other => Some(other.to_string()),
        });
    if status_hit || path_msg.is_some() {
        Some(path_msg.unwrap_or_else(|| format!("HTTP {status}")))
    } else {
        None
    }
}

fn extract_rows(body: &Value, path: &str) -> datafusion::common::Result<Vec<Value>> {
    if path == "$" {
        return as_array(body);
    }
    // Simple `$.field.subfield` walker; no filters, slices, or wildcards.
    let trimmed = path.trim_start_matches("$.");
    let mut current = body;
    for part in trimmed.split('.') {
        if part.is_empty() {
            continue;
        }
        let Some(next) = current.get(part) else {
            return Ok(Vec::new());
        };
        current = next;
    }
    as_array(current)
}

fn as_array(v: &Value) -> datafusion::common::Result<Vec<Value>> {
    match v {
        Value::Array(a) => Ok(a.clone()),
        Value::Null => Ok(Vec::new()),
        other => Err(DataFusionError::Plan(format!(
            "expected array at response path, got {other:?}"
        ))),
    }
}

fn build_batch(
    schema: &SchemaRef,
    columns: &[ResponseColumn],
    rows: &[Value],
    params: &BTreeMap<String, String>,
) -> datafusion::common::Result<RecordBatch> {
    let n = rows.len();
    let mut arrays: Vec<ArrayRef> = Vec::with_capacity(columns.len());
    for col in columns {
        // `json` columns keep the value as raw JSON text, regardless of whether
        // it's a scalar, object, or array.
        if col.r#type.eq_ignore_ascii_case("json") {
            let mut b = StringBuilder::with_capacity(n, n * 16);
            for row in rows {
                match pull_value(row, col, params) {
                    Some(Value::Null) | None => b.append_null(),
                    Some(v) => b.append_value(serde_json::to_string(&v).unwrap_or_default()),
                }
            }
            arrays.push(Arc::new(b.finish()) as ArrayRef);
            continue;
        }
        let array = match schema.field_with_name(&col.name) {
            Ok(f) => match f.data_type() {
                DataType::Utf8 => {
                    let mut b = StringBuilder::with_capacity(n, n * 8);
                    for row in rows {
                        let v = pull_value(row, col, params);
                        match v {
                            Some(Value::String(s)) => b.append_value(&s),
                            Some(Value::Number(n)) => b.append_value(n.to_string()),
                            Some(Value::Bool(bo)) => b.append_value(bo.to_string()),
                            Some(Value::Null) | None => b.append_null(),
                            Some(other) => b.append_value(other.to_string()),
                        }
                    }
                    Arc::new(b.finish()) as ArrayRef
                }
                DataType::Int64 => {
                    let mut b = Int64Builder::with_capacity(n);
                    for row in rows {
                        match pull_value(row, col, params) {
                            Some(Value::Number(n)) => match n.as_i64() {
                                Some(i) => b.append_value(i),
                                None => b.append_null(),
                            },
                            Some(Value::String(s)) => match s.parse::<i64>() {
                                Ok(i) => b.append_value(i),
                                Err(_) => b.append_null(),
                            },
                            _ => b.append_null(),
                        }
                    }
                    Arc::new(b.finish()) as ArrayRef
                }
                DataType::Int32 => {
                    let mut b = Int32Builder::with_capacity(n);
                    for row in rows {
                        match pull_value(row, col, params) {
                            Some(Value::Number(n)) => {
                                match n.as_i64().and_then(|x| i32::try_from(x).ok()) {
                                    Some(i) => b.append_value(i),
                                    None => b.append_null(),
                                }
                            }
                            Some(Value::String(s)) => match s.parse::<i32>() {
                                Ok(i) => b.append_value(i),
                                Err(_) => b.append_null(),
                            },
                            _ => b.append_null(),
                        }
                    }
                    Arc::new(b.finish()) as ArrayRef
                }
                DataType::Float64 => {
                    let mut b = Float64Builder::with_capacity(n);
                    for row in rows {
                        match pull_value(row, col, params) {
                            Some(Value::Number(n)) => match n.as_f64() {
                                Some(f) => b.append_value(f),
                                None => b.append_null(),
                            },
                            _ => b.append_null(),
                        }
                    }
                    Arc::new(b.finish()) as ArrayRef
                }
                DataType::Boolean => {
                    let values: Vec<Option<bool>> = rows
                        .iter()
                        .map(|r| match pull_value(r, col, params) {
                            Some(Value::Bool(b)) => Some(b),
                            _ => None,
                        })
                        .collect();
                    Arc::new(BooleanArray::from(values)) as ArrayRef
                }
                DataType::Timestamp(TimeUnit::Microsecond, _) => {
                    let mut b = TimestampMicrosecondBuilder::with_capacity(n);
                    for row in rows {
                        match pull_value(row, col, params).and_then(|v| parse_timestamp_micros(&v))
                        {
                            Some(t) => b.append_value(t),
                            None => b.append_null(),
                        }
                    }
                    Arc::new(b.finish().with_timezone_opt(timestamp_tz(f.data_type()))) as ArrayRef
                }
                DataType::Date32 => {
                    let mut b = Date32Builder::with_capacity(n);
                    for row in rows {
                        match pull_value(row, col, params).and_then(|v| parse_date32(&v)) {
                            Some(d) => b.append_value(d),
                            None => b.append_null(),
                        }
                    }
                    Arc::new(b.finish()) as ArrayRef
                }
                _ => {
                    // Fallback: stringify whatever we have.
                    let mut b = StringBuilder::with_capacity(n, n * 8);
                    for _ in rows {
                        b.append_null();
                    }
                    Arc::new(b.finish()) as ArrayRef
                }
            },
            Err(_) => {
                let mut b = StringBuilder::with_capacity(n, n * 8);
                for _ in rows {
                    b.append_null();
                }
                Arc::new(b.finish()) as ArrayRef
            }
        };
        arrays.push(array);
    }
    RecordBatch::try_new(schema.clone(), arrays)
        .map_err(|e| DataFusionError::ArrowError(Box::new(e), None))
}

/// Extract the timezone (if any) from a `Timestamp` data type.
fn timestamp_tz(dt: &DataType) -> Option<std::sync::Arc<str>> {
    match dt {
        DataType::Timestamp(_, tz) => tz.clone(),
        _ => None,
    }
}

/// Parse an ISO-8601 / RFC 3339 string into microseconds since the Unix epoch.
/// Numbers and unparseable strings yield `None` (the cell becomes null).
fn parse_timestamp_micros(v: &Value) -> Option<i64> {
    let s = v.as_str()?;
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp_micros());
    }
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
    ] {
        if let Ok(ndt) = chrono::NaiveDateTime::parse_from_str(s, fmt) {
            return Some(ndt.and_utc().timestamp_micros());
        }
    }
    None
}

/// Parse a `YYYY-MM-DD` string into days since the Unix epoch (Arrow `Date32`).
fn parse_date32(v: &Value) -> Option<i32> {
    let s = v.as_str()?;
    let date = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?;
    let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1)?;
    i32::try_from((date - epoch).num_days()).ok()
}

fn pull_value(
    row: &Value,
    col: &ResponseColumn,
    params: &BTreeMap<String, String>,
) -> Option<Value> {
    match col.source.as_deref() {
        None => row.get(&col.name).cloned(),
        Some("param") => params.get(&col.name).cloned().map(Value::String),
        // `$` alone is the whole row element (raw passthrough, typically a `json` column).
        Some("$") => Some(row.clone()),
        Some(path) if path.starts_with('$') => {
            let trimmed = path.trim_start_matches("$.");
            let mut current: &Value = row;
            for part in trimmed.split('.') {
                if part.is_empty() {
                    continue;
                }
                current = current.get(part)?;
            }
            Some(current.clone())
        }
        Some(other) => row.get(other).cloned(),
    }
}
