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
    builder::{Float64Builder, Int32Builder, Int64Builder, StringBuilder},
};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
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
use crate::source::{AuthSpec, HttpSource, HttpTableSpec, ResponseColumn, schema_for};

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
                if let Some((col, _)) = extract_eq_literal(f)
                    && self.spec.params.iter().any(|p| p.name == col)
                {
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
        _limit: Option<usize>,
    ) -> datafusion::common::Result<Arc<dyn ExecutionPlan>> {
        let _ = state;
        let mut params: BTreeMap<String, String> = BTreeMap::new();
        for f in filters {
            if let Some((col, val)) = extract_eq_literal(f)
                && self.spec.params.iter().any(|p| p.name == col)
            {
                params.insert(col, val);
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

        let method = self.spec.method.parse().unwrap_or(reqwest::Method::GET);

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

            // Rate limit: wait for a permit before each request.
            if let Some(limiter) = &self.source.limiter {
                limiter.until_ready().await;
            }

            let url = match &next_url {
                Some(u) => u.clone(),
                None => build_url(&self.source.base_url, &self.spec.endpoint, &next_params)?,
            };

            let mut req = self.source.client.request(method.clone(), url);
            for (k, v) in &self.source.headers {
                req = req.header(k, v);
            }
            for (k, v) in &self.spec.headers {
                req = req.header(k, v);
            }
            match &self.source.auth {
                AuthSpec::None => {}
                AuthSpec::Bearer { token } => {
                    req = req.bearer_auth(token);
                }
                AuthSpec::ApiKey { header, value } => {
                    req = req.header(header, value);
                }
                AuthSpec::Basic { username, password } => {
                    req = req.basic_auth(username, Some(password));
                }
            }

            let resp = send_with_retry(req, &self.source.retry).await?;
            let status = resp.status();
            let headers = resp.headers().clone();
            let body: Value = resp.json().await.map_err(|e| {
                DataFusionError::External(Box::new(std::io::Error::other(format!(
                    "json parse failed (status {status}): {e}"
                ))))
            })?;

            let rows = extract_rows(&body, &self.spec.response.path)?;
            let row_count = rows.len();
            all_rows.extend(rows);

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
/// (`base * 2^attempt`, capped at `max_backoff`). For 429/503, honors a numeric
/// `Retry-After` header (seconds) when present, otherwise falls back to the
/// backoff. After exhausting `max_retries`, returns the last error wrapped as a
/// `DataFusionError::External`.
async fn send_with_retry(
    req: reqwest::RequestBuilder,
    retry: &crate::source::RetryConfig,
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
                let retryable = status.is_server_error()
                    || status == StatusCode::TOO_MANY_REQUESTS
                    || status == StatusCode::SERVICE_UNAVAILABLE;
                if !retryable || attempt >= retry.max_retries {
                    return Ok(resp);
                }
                // Prefer an explicit Retry-After (seconds) for 429/503.
                let wait_ms = if status == StatusCode::TOO_MANY_REQUESTS
                    || status == StatusCode::SERVICE_UNAVAILABLE
                {
                    retry_after_ms(&resp).unwrap_or_else(|| backoff_ms(retry, attempt))
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

fn extract_eq_literal(expr: &Expr) -> Option<(String, String)> {
    use datafusion::logical_expr::{BinaryExpr, Operator};
    use datafusion::scalar::ScalarValue;
    if let Expr::BinaryExpr(BinaryExpr { left, op, right }) = expr
        && matches!(op, Operator::Eq)
    {
        let (col, scalar) = match (left.as_ref(), right.as_ref()) {
            (Expr::Column(c), Expr::Literal(s, _)) => (c, s),
            (Expr::Literal(s, _), Expr::Column(c)) => (c, s),
            _ => return None,
        };
        let value = match scalar {
            ScalarValue::Utf8(Some(s)) | ScalarValue::LargeUtf8(Some(s)) => s.clone(),
            ScalarValue::Int32(Some(n)) => n.to_string(),
            ScalarValue::Int64(Some(n)) => n.to_string(),
            ScalarValue::UInt64(Some(n)) => n.to_string(),
            ScalarValue::Boolean(Some(b)) => b.to_string(),
            _ => return None,
        };
        return Some((col.name.clone(), value));
    }
    None
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

fn pull_value(
    row: &Value,
    col: &ResponseColumn,
    params: &BTreeMap<String, String>,
) -> Option<Value> {
    match col.source.as_deref() {
        None => row.get(&col.name).cloned(),
        Some("param") => params.get(&col.name).cloned().map(Value::String),
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
