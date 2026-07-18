//! Typed HTTP table provider: one declared endpoint with declared columns.
//!
//! Simplifications:
//! - Filter pushdown is done by lifting `WHERE col = literal` filters that
//!   match a declared parameter and substituting them into the URL path /
//!   query string.
//! - Pagination follows the table's `PaginationConfig`; absent config means a
//!   single-page fetch.
//! - Required params must appear as `WHERE col = value` filters; otherwise
//!   the scan returns a clear error.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use arrow_array::{
    ArrayRef, BooleanArray, RecordBatch, RecordBatchOptions,
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
    BodyKind, HttpSource, HttpTableSpec, PaginationConfig, RateLimitPolicy, RequestBody, Reshape,
    ResponseColumn, ResponseSpec, custom_body_object, effective_columns, schema_for,
};

/// Backstop page cap when a source declares no `safety.max_pages`, so a
/// misbehaving API can't paginate without bound. Raise it via `safety.max_pages`.
const DEFAULT_MAX_PAGES: u32 = 1000;

#[derive(Debug)]
pub struct HttpTableProvider {
    pub source: Arc<HttpSource>,
    pub spec: Arc<HttpTableSpec>,
    pub schema: SchemaRef,
    /// Output columns the batch builder emits — `response.schema` plus any
    /// `filterable` params. Kept in lockstep with `schema` (both come from
    /// [`effective_columns`]) so the built batch always matches the Arrow schema.
    pub out_columns: Vec<ResponseColumn>,
    /// Hard cap on pagination calls, threaded from the table/source safety
    /// policy. `None` falls back to [`DEFAULT_MAX_PAGES`].
    pub max_pages: Option<u32>,
    /// Hard cap on rows materialised by a scan, threaded from the table/source
    /// safety policy. `None` means no cap.
    pub max_rows: Option<u64>,
}

impl pawrly_core::DynamicFilterCapable for HttpTableProvider {
    fn dynamic_filter_columns(&self) -> Vec<String> {
        // Declared params can absorb runtime `IN(...)` filters on equality.
        self.spec.params.iter().map(|p| p.name.clone()).collect()
    }
}

impl HttpTableProvider {
    pub fn new(source: Arc<HttpSource>, spec: Arc<HttpTableSpec>) -> Self {
        Self::with_safety(source, spec, None, None)
    }

    pub fn with_safety(
        source: Arc<HttpSource>,
        spec: Arc<HttpTableSpec>,
        max_pages: Option<u32>,
        max_rows: Option<u64>,
    ) -> Self {
        let schema = schema_for(&spec);
        let out_columns = effective_columns(&spec);
        Self {
            source,
            spec,
            schema,
            out_columns,
            max_pages,
            max_rows,
        }
    }

    /// Whether a filter can be pushed into the request: an equality on a
    /// declared param, a comparison the param's `accepts`/`emit` covers, or an
    /// `IN (...)` on a param that either explodes into repeated query pairs or
    /// fans out into one request per value.
    fn can_push_down(&self, expr: &Expr) -> bool {
        // `col IN (...)` pushes down when the param opts into `explode` (repeated
        // `?k=a&k=b` query pairs in one request) or is a path placeholder / a
        // `required` param (fanned out to one request per value, like the raw
        // table). Otherwise DataFusion keeps the filter above the scan.
        if let Some((col, _)) = extract_in_list(expr) {
            return self
                .spec
                .params
                .iter()
                .any(|p| p.name == col && (p.explode || p.required || self.is_path_param(&col)));
        }
        let Some((col, op, _)) = extract_cmp(expr) else {
            return false;
        };
        let Some(param) = self.spec.params.iter().find(|p| p.name == col) else {
            return false;
        };
        op == "=" || (param.accepts.iter().any(|a| a == op) && param.emit.contains_key(op))
    }

    /// Whether `name` appears as a `{name}` placeholder in the table's endpoint
    /// (the default or any conditional request) — i.e. it is substituted into the
    /// URL path rather than sent as a query parameter. A path param can never be
    /// satisfied by repeated query pairs, so an `IN (...)` on one must fan out.
    fn is_path_param(&self, name: &str) -> bool {
        let needle = format!("{{{name}}}");
        self.spec.endpoint.contains(&needle)
            || self
                .spec
                .requests
                .iter()
                .any(|r| r.endpoint.contains(&needle))
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

    /// Run the full paginated fetch for one fully-bound parameter set, returning
    /// the collected JSON rows (truncated to `limit`). This is the per-request
    /// unit `scan` drives once for a normal query, and once per value when an
    /// `IN (...)` on a path / required param fans out.
    async fn fetch_rows(
        &self,
        params: &BTreeMap<String, String>,
        explode_params: &BTreeMap<String, Vec<String>>,
        limit: Option<usize>,
    ) -> datafusion::common::Result<Vec<Value>> {
        // Pick the request shape (endpoint/method/body) for this binding.
        let (endpoint, method_str, body) = self.select_request(params);
        let method = method_str.parse().unwrap_or(reqwest::Method::GET);
        let body_template = body.map(|b| b.template.as_str());

        // Body-cursor pagination injects the cursor into the request body at this
        // path; `body_cursor` carries the value for the upcoming page.
        let body_cursor_path = match &self.spec.pagination {
            Some(PaginationConfig::BodyCursor { cursor_path, .. }) => Some(cursor_path.as_str()),
            _ => None,
        };
        let mut body_cursor: Option<String> = None;

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
            // Enforce the page cap before issuing the request for this page. An
            // unconfigured source falls back to DEFAULT_MAX_PAGES so a misbehaving
            // API (e.g. a never-terminating cursor) can't paginate unboundedly.
            let max_pages = self.max_pages.unwrap_or(DEFAULT_MAX_PAGES);
            if page_index as u64 >= max_pages as u64 {
                let err = pawrly_core::SafetyError::TooManyPages {
                    table: self.spec.name.clone(),
                    max_pages,
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
                None => build_url(
                    &self.source.base_url,
                    &self.source.allowed_hosts,
                    endpoint,
                    &next_params,
                    body_template,
                    explode_params,
                )?,
            };

            let mut req = self.source.client.request(method.clone(), url);
            // Source-level headers first, then per-table headers. A per-table
            // header overrides a source-level one on a (case-insensitive) key
            // collision — so skip any source header the table also sets, since
            // reqwest's `header()` *appends* rather than replaces.
            for (k, v) in &self.source.headers {
                let overridden = self
                    .spec
                    .headers
                    .keys()
                    .any(|tk| tk.eq_ignore_ascii_case(k.as_str()));
                if !overridden {
                    req = req.header(k, v);
                }
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
                    let mut rendered = match tbody.kind {
                        BodyKind::Json => {
                            render_json_body(&tbody.template, &next_params, &self.spec.params)
                        }
                        BodyKind::Form => render_template(&tbody.template, &next_params),
                    };
                    // Inject the body-cursor for the next page, if any.
                    if let (Some(cursor), Some(path)) = (&body_cursor, body_cursor_path)
                        && let Ok(mut v) = serde_json::from_str::<Value>(&rendered)
                        && paginate::set_json_at_path(&mut v, path, Value::String(cursor.clone()))
                    {
                        rendered = v.to_string();
                    }
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
                    let rendered =
                        render_json_body(&tbody.template, &next_params, &self.spec.params);
                    let mut value: Value = serde_json::from_str(&rendered).map_err(|e| {
                        DataFusionError::Plan(format!("table body is not valid JSON: {e}"))
                    })?;
                    let obj = value.as_object_mut().ok_or_else(|| {
                        DataFusionError::Plan(
                            "table body must be a JSON object to merge custom auth body".into(),
                        )
                    })?;
                    obj.extend(custom_body_object(auth_body));
                    if let (Some(cursor), Some(path)) = (&body_cursor, body_cursor_path) {
                        paginate::set_json_at_path(&mut value, path, Value::String(cursor.clone()));
                    }
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
            let body_json: Value = match serde_json::from_str(&text) {
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
                && let Some(msg) = detect_error(spec, status_code, &body_json)
            {
                return Err(scan_error(msg));
            }

            let page_start = all_rows.len();
            all_rows.extend(extract_rows(&body_json, &self.spec.response)?);

            // Stop early once enough rows are collected to satisfy a LIMIT.
            if let Some(lim) = limit
                && all_rows.len() >= lim
            {
                break;
            }

            // Safety ceiling: refuse a scan that materialises more than `max_rows`
            // rows (a LIMIT at or under the cap has already broken out above).
            if let Some(max) = self.max_rows
                && all_rows.len() as u64 > max
            {
                let err = pawrly_core::SafetyError::TooManyRows {
                    table: self.spec.name.clone(),
                    max_rows: max,
                };
                return Err(DataFusionError::External(Box::new(std::io::Error::other(
                    err.to_string(),
                ))));
            }

            // Without a pagination config we fetch exactly one page.
            let Some(config) = &self.spec.pagination else {
                break;
            };

            let page_rows = &all_rows[page_start..];
            match paginate::next_page(
                config,
                &next_params,
                &body_json,
                &headers,
                page_rows,
                page_index,
            ) {
                Some(NextPage::Params(p)) => {
                    next_params = p;
                    next_url = None;
                }
                Some(NextPage::Url(u)) => {
                    let parsed = url::Url::parse(&u).map_err(|e| {
                        DataFusionError::Plan(format!("bad pagination url `{u}`: {e}"))
                    })?;
                    crate::guard::check_target(
                        &parsed,
                        &self.source.base_url,
                        &self.source.allowed_hosts,
                    )
                    .map_err(DataFusionError::Plan)?;
                    next_url = Some(parsed);
                }
                Some(NextPage::BodyCursor(c)) => {
                    body_cursor = Some(c);
                    next_url = None;
                }
                None => break,
            }
            page_index += 1;
        }

        if let Some(lim) = limit {
            all_rows.truncate(lim);
        }
        Ok(all_rows)
    }

    /// Resolve filters into request params, fetch (fanning out `IN (...)` on a
    /// path / required param), and build the result batches — the Session-free
    /// core shared by [`TableProvider::scan`] and the dependent-join bind step.
    ///
    /// `allow_defer` controls the unbound-required-param case: `true` returns a
    /// [`DeferredHttpScanExec`] placeholder (the scan's required column becomes a
    /// candidate join key); `false` fails with `PAWRLY_SAFETY_REQUIRED_FILTER`.
    /// The dependent-join operator calls this with the key bound and
    /// `allow_defer = false`, so it never re-defers.
    pub async fn scan_bound(
        &self,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
        allow_defer: bool,
    ) -> datafusion::common::Result<Arc<dyn ExecutionPlan>> {
        // Equality / comparison filters bind their param directly.
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

        // `IN (...)` filters split two ways: an `explode` query param repeats as
        // `?k=a&k=b` within a single request; a path placeholder or a `required`
        // param fans out to one request per value (the raw table's behavior),
        // with the results unioned.
        let mut explode_params: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut fanout: Vec<(String, Vec<String>)> = Vec::new();
        for f in filters {
            let Some((col, vals)) = extract_in_list(f) else {
                continue;
            };
            let Some(param) = self.spec.params.iter().find(|p| p.name == col) else {
                continue;
            };
            if param.explode && !self.is_path_param(&col) {
                explode_params.insert(col, vals);
            } else if param.required || self.is_path_param(&col) {
                fanout.push((col, vals));
            }
        }

        // Defaults for params the query left unbound (and didn't explode/fan out).
        for p in &self.spec.params {
            if let Some(default) = &p.default
                && !params.contains_key(&p.name)
                && !bound_elsewhere(&p.name, &explode_params, &fanout)
            {
                params.insert(p.name.clone(), default.clone());
            }
        }
        // Derived params: compute from the clock or another (already bound) param
        // when the query didn't supply a value.
        let now_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        let mut derived: Vec<(String, String)> = Vec::new();
        for p in &self.spec.params {
            if params.contains_key(&p.name) || bound_elsewhere(&p.name, &explode_params, &fanout) {
                continue;
            }
            if let Some(d) = &p.derive
                && let Some(v) = d.resolve(&params, now_epoch)
            {
                derived.push((p.name.clone(), v));
            }
        }
        params.extend(derived);

        // Required params must be bound directly, exploded, or fanned out. Any
        // still-unbound required params are the dependent-join key candidates.
        let unbound: Vec<String> = self
            .spec
            .params
            .iter()
            .filter(|p| {
                p.required
                    && !params.contains_key(&p.name)
                    && !bound_elsewhere(&p.name, &explode_params, &fanout)
            })
            .map(|p| p.name.clone())
            .collect();
        if let Some(first) = unbound.first() {
            if allow_defer {
                return Ok(Arc::new(crate::deferred::DeferredHttpScanExec::new(
                    self, projection, filters, unbound,
                )));
            }
            return Err(DataFusionError::Plan(format!(
                "table `{}` requires filter `{} = ...` (PAWRLY_SAFETY_REQUIRED_FILTER)",
                self.spec.name, first
            )));
        }

        // One binding set per fan-out combination (the Cartesian product of the
        // fan-out params' value lists); a query with no fan-out yields exactly one.
        let binding_sets = expand_fanout(&params, &fanout);

        // Fetch each binding set, accumulating batches and stopping once `limit`
        // rows are collected across the whole union.
        let mut out_batches: Vec<RecordBatch> = Vec::new();
        let mut total_rows: usize = 0;
        for bind in &binding_sets {
            if let Some(l) = limit
                && total_rows >= l
            {
                break;
            }
            let remaining = limit.map(|l| l.saturating_sub(total_rows));
            let rows = self.fetch_rows(bind, &explode_params, remaining).await?;
            // Safety ceiling across the union of fan-out requests.
            if let Some(max) = self.max_rows
                && (total_rows as u64).saturating_add(rows.len() as u64) > max
            {
                let err = pawrly_core::SafetyError::TooManyRows {
                    table: self.spec.name.clone(),
                    max_rows: max,
                };
                return Err(DataFusionError::External(Box::new(std::io::Error::other(
                    err.to_string(),
                ))));
            }
            if rows.is_empty() {
                continue;
            }
            let mut batch = build_batch(&self.schema, &self.out_columns, &rows, bind)?;
            // Trim the final batch so the union never overshoots the LIMIT.
            if let Some(l) = limit
                && total_rows + batch.num_rows() > l
            {
                batch = batch.slice(0, l - total_rows);
            }
            total_rows += batch.num_rows();
            if batch.num_rows() > 0 {
                out_batches.push(batch);
            }
        }
        // Always emit at least one (possibly empty) batch so the schema is present.
        if out_batches.is_empty() {
            out_batches.push(build_batch(&self.schema, &self.out_columns, &[], &params)?);
        }

        let projected_schema = if let Some(p) = projection {
            let fields: Vec<Field> = p.iter().map(|i| self.schema.field(*i).clone()).collect();
            Arc::new(Schema::new(fields))
        } else {
            self.schema.clone()
        };
        let projected_batches: Vec<RecordBatch> = out_batches
            .iter()
            .map(|b| project_batch(b, projection, &projected_schema))
            .collect::<datafusion::common::Result<Vec<_>>>()?;

        let exec = MemorySourceConfig::try_new_exec(&[projected_batches], projected_schema, None)
            .map_err(|e| DataFusionError::Plan(e.to_string()))?;
        Ok(exec)
    }

    /// Fetch one fully-bound parameter set and build the result batch — the
    /// `fetch_rows` + `build_batch` core shared with [`HttpTableProvider::scan_bound`],
    /// but with params already bound from a function's call arguments (no filter
    /// resolution, fan-out, or projection). Used by the HTTP function executor.
    pub(crate) async fn scan_params(
        &self,
        params: &BTreeMap<String, String>,
        limit: Option<usize>,
    ) -> datafusion::common::Result<RecordBatch> {
        let rows = self.fetch_rows(params, &BTreeMap::new(), limit).await?;
        build_batch(&self.schema, &self.out_columns, &rows, params)
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
        // `allow_defer = true`: an unbound required param yields a
        // `DeferredHttpScanExec` placeholder (so a dependent join can bind it at
        // runtime) rather than failing the plan outright.
        self.scan_bound(projection, filters, limit, true).await
    }
}

/// Send a request with retry/backoff, recording the `pawrly.source.request.*`
/// metrics for the whole logical request (across retries). `kind` and `status`
/// are recorded; source-name attribution is not available at this layer.
#[tracing::instrument(name = "pawrly.source.http.request", skip_all)]
async fn send_with_retry(
    req: reqwest::RequestBuilder,
    retry: &crate::source::RetryConfig,
    rate_limit: &RateLimitPolicy,
) -> datafusion::common::Result<reqwest::Response> {
    let started = std::time::Instant::now();
    let result = send_with_retry_inner(req, retry, rate_limit).await;

    let status = if result.is_ok() { "ok" } else { "error" };
    let code = result.as_ref().map_or(0, |r| r.status().as_u16());
    pawrly_telemetry::metrics::source_request_total().add(
        1,
        &[
            opentelemetry::KeyValue::new("kind", "http"),
            opentelemetry::KeyValue::new("status", status),
            opentelemetry::KeyValue::new("http.response.status_code", i64::from(code)),
        ],
    );
    pawrly_telemetry::metrics::source_request_duration().record(
        started.elapsed().as_secs_f64() * 1000.0,
        &[
            opentelemetry::KeyValue::new("kind", "http"),
            opentelemetry::KeyValue::new("status", status),
        ],
    );
    result
}

/// Retries on transport errors and HTTP 5xx with exponential backoff
/// (`base * 2^attempt`, capped at `max_backoff`). For 429/503 and any
/// `rate_limit.extra_statuses` (e.g. GitHub's `403`), prefers the reset header
/// (epoch seconds) then a numeric `Retry-After` (seconds), otherwise falls back
/// to the backoff. After exhausting `max_retries`, returns the last error
/// wrapped as a `DataFusionError::External`.
async fn send_with_retry_inner(
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
                        "http request failed: {}",
                        error_chain(&e)
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
                // A refused redirect is a policy decision, not a transient failure — don't retry.
                if e.is_redirect() || attempt >= retry.max_retries {
                    return Err(DataFusionError::External(Box::new(std::io::Error::other(
                        format!("http request failed: {}", error_chain(&e)),
                    ))));
                }
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms(retry, attempt)))
                    .await;
            }
        }
        attempt += 1;
    }
}

/// reqwest's `Display` omits the cause for redirect-policy refusals; append
/// the source chain so the reason reaches the user.
fn error_chain(e: &reqwest::Error) -> String {
    use std::error::Error as _;
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(inner) = src {
        let inner_s = inner.to_string();
        if !s.contains(&inner_s) {
            s.push_str(": ");
            s.push_str(&inner_s);
        }
        src = inner.source();
    }
    s
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

/// Sentinel marking a JSON value that came from an unbound optional param, so
/// [`prune_unbound_filters`] can drop the enclosing object member.
const UNBOUND_SENTINEL: &str = "__pawrly_unbound_placeholder__";

/// Render a **JSON** body, then drop any object member still tied to an *unbound*
/// declared param.
///
/// A GraphQL filter template inlines optional filters (`{"id": {"eq": "{x}"}}`).
/// When `x` is neither filtered nor defaulted, plain [`render_template`] leaves
/// the placeholder literal — surviving as the string `"{x}"` (matches nothing)
/// or, for an unquoted numeric/bool slot, as invalid JSON. GraphQL wants the key
/// *absent* to mean "no filter", so here each unbound placeholder is replaced
/// with a sentinel, the body is parsed, and every member resolving to that
/// sentinel (and any object it empties) is removed. Falls back to the plain
/// render if the result isn't parseable, so a non-JSON or malformed body keeps
/// today's behaviour.
fn render_json_body(
    template: &str,
    bound: &BTreeMap<String, String>,
    declared: &[crate::source::ParamSpec],
) -> String {
    let rendered = render_template(template, bound);
    let unbound: Vec<&str> = declared
        .iter()
        .filter(|p| !bound.contains_key(&p.name))
        .map(|p| p.name.as_str())
        .collect();
    if unbound.is_empty() {
        return rendered;
    }
    let sentinel_json = format!("\"{UNBOUND_SENTINEL}\"");
    let mut marked = rendered.clone();
    for name in unbound {
        // Quoted slot first (`"{name}"`), then any bare slot (`{name}`), so both
        // string and numeric/bool placeholders become a valid sentinel string.
        marked = marked.replace(&format!("\"{{{name}}}\""), &sentinel_json);
        marked = marked.replace(&format!("{{{name}}}"), &sentinel_json);
    }
    match serde_json::from_str::<Value>(&marked) {
        Ok(v) => {
            prune_unbound_filters(v).map_or_else(|| "{}".to_string(), |pruned| pruned.to_string())
        }
        Err(_) => rendered,
    }
}

/// Drop values that are the unbound sentinel, and any object that empties out as
/// a result. Returns `None` when the whole value collapses (a member to remove).
fn prune_unbound_filters(value: Value) -> Option<Value> {
    match value {
        Value::String(s) if s == UNBOUND_SENTINEL => None,
        Value::Object(map) => {
            let had_members = !map.is_empty();
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                if let Some(pruned) = prune_unbound_filters(v) {
                    out.insert(k, pruned);
                }
            }
            // An object emptied *by pruning* is itself a dead filter; an
            // originally-empty object is preserved as-is.
            if had_members && out.is_empty() {
                None
            } else {
                Some(Value::Object(out))
            }
        }
        Value::Array(arr) => Some(Value::Array(
            arr.into_iter().filter_map(prune_unbound_filters).collect(),
        )),
        other => Some(other),
    }
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

/// Whether `name` is already satisfied by an `explode` (repeated query pairs) or
/// a fan-out (one request per value) binding, so it shouldn't also be defaulted,
/// derived, or flagged as a missing required param.
fn bound_elsewhere(
    name: &str,
    explode: &BTreeMap<String, Vec<String>>,
    fanout: &[(String, Vec<String>)],
) -> bool {
    explode.contains_key(name) || fanout.iter().any(|(n, _)| n == name)
}

/// Expand the fan-out params into one fully-bound parameter set per combination
/// (the Cartesian product of their value lists), each layered over `base`. With
/// no fan-out params this returns exactly `[base]`, so the non-fan-out path stays
/// a single request.
fn expand_fanout(
    base: &BTreeMap<String, String>,
    fanout: &[(String, Vec<String>)],
) -> Vec<BTreeMap<String, String>> {
    let mut sets = vec![base.clone()];
    for (name, vals) in fanout {
        let mut next = Vec::with_capacity(sets.len().saturating_mul(vals.len()));
        for s in &sets {
            for v in vals {
                let mut m = s.clone();
                m.insert(name.clone(), v.clone());
                next.push(m);
            }
        }
        sets = next;
    }
    sets
}

/// Apply a column projection to a batch, preserving the row count for
/// zero-column projections (e.g. `COUNT(*)`). A `None` projection clones as-is.
fn project_batch(
    batch: &RecordBatch,
    projection: Option<&Vec<usize>>,
    projected_schema: &SchemaRef,
) -> datafusion::common::Result<RecordBatch> {
    match projection {
        Some(p) => {
            let cols: Vec<ArrayRef> = p.iter().map(|i| batch.column(*i).clone()).collect();
            let options = RecordBatchOptions::new().with_row_count(Some(batch.num_rows()));
            RecordBatch::try_new_with_options(projected_schema.clone(), cols, &options)
                .map_err(|e| DataFusionError::ArrowError(Box::new(e), None))
        }
        None => Ok(batch.clone()),
    }
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
    allowed: &[crate::guard::HostPattern],
    endpoint: &str,
    params: &BTreeMap<String, String>,
    body_template: Option<&str>,
    explode: &BTreeMap<String, Vec<String>>,
) -> datafusion::common::Result<url::Url> {
    let mut path = endpoint.to_string();
    let mut query_params: Vec<(String, String)> = Vec::new();
    for (k, v) in params {
        let needle = format!("{{{k}}}");
        if path.contains(&needle) {
            path = path.replace(&needle, v);
        } else if body_template.is_some_and(|t| t.contains(&needle)) {
            // A param consumed by the request body is not also a query param.
        } else {
            query_params.push((k.clone(), v.clone()));
        }
    }
    // Explode `IN (...)` filters into repeated query pairs (`?k=a&k=b`).
    for (k, vals) in explode {
        for v in vals {
            query_params.push((k.clone(), v.clone()));
        }
    }
    let trimmed = path.trim_start_matches('/');
    let mut url = base
        .join(trimmed)
        .map_err(|e| DataFusionError::Plan(format!("bad url: {e}")))?;
    crate::guard::check_target(&url, base, allowed).map_err(DataFusionError::Plan)?;
    if !query_params.is_empty() {
        let mut q = url.query_pairs_mut();
        for (k, v) in &query_params {
            q.append_pair(k, v);
        }
    }
    Ok(url)
}

/// Extract `column IN (lit, lit, …)` (non-negated, all literals) as the column
/// name plus its values as strings. Returns `None` for anything else.
///
/// Recognises both the literal `InList` node and the disjunction-of-equalities
/// form (`col = a OR col = b OR …`) that DataFusion lowers a short `IN (…)` into
/// before filter pushdown — without the latter, `explode` would never fire for a
/// SQL `IN`.
fn extract_in_list(expr: &Expr) -> Option<(String, Vec<String>)> {
    match expr {
        Expr::InList(il) if !il.negated => {
            let Expr::Column(c) = il.expr.as_ref() else {
                return None;
            };
            let mut vals = Vec::with_capacity(il.list.len());
            for item in &il.list {
                let Expr::Literal(s, _) = item else {
                    return None;
                };
                vals.push(scalar_to_string(s)?);
            }
            Some((c.name.clone(), vals))
        }
        Expr::BinaryExpr(_) => {
            let mut col: Option<String> = None;
            let mut vals = Vec::new();
            collect_or_equalities(expr, &mut col, &mut vals).then_some(())?;
            // Require at least two values so a lone `col = a` stays an equality
            // (handled by `extract_cmp`), not a single-element explode.
            if vals.len() < 2 {
                return None;
            }
            Some((col?, vals))
        }
        _ => None,
    }
}

/// Walk a disjunction of `col = literal` equalities on a *single* column,
/// accumulating the literals. Returns `false` if any leaf isn't an equality on
/// that one column, or the tree contains a non-`OR` connective.
fn collect_or_equalities(expr: &Expr, col: &mut Option<String>, vals: &mut Vec<String>) -> bool {
    use datafusion::logical_expr::{BinaryExpr, Operator};
    let Expr::BinaryExpr(BinaryExpr { left, op, right }) = expr else {
        return false;
    };
    match op {
        Operator::Or => {
            collect_or_equalities(left, col, vals) && collect_or_equalities(right, col, vals)
        }
        Operator::Eq => {
            let (c, scalar) = match (left.as_ref(), right.as_ref()) {
                (Expr::Column(c), Expr::Literal(s, _)) => (c, s),
                (Expr::Literal(s, _), Expr::Column(c)) => (c, s),
                _ => return false,
            };
            match col {
                Some(existing) if existing != &c.name => return false,
                _ => *col = Some(c.name.clone()),
            }
            match scalar_to_string(scalar) {
                Some(v) => {
                    vals.push(v);
                    true
                }
                None => false,
            }
        }
        _ => false,
    }
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

fn extract_rows(body: &Value, spec: &ResponseSpec) -> datafusion::common::Result<Vec<Value>> {
    let Some(at) = paginate::json_at_path(body, &spec.path) else {
        return Ok(Vec::new());
    };
    match &spec.reshape {
        None => as_array(at),
        Some(Reshape::DictEntries) => Ok(reshape_dict_entries(at)),
        Some(Reshape::SeriesPoints {
            series,
            points,
            timestamp,
            value,
        }) => Ok(reshape_series_points(at, series, points, timestamp, value)),
    }
}

/// One row per entry of the object at the response path; the entry value with
/// its key added as `_key` (or `{_key, _value}` when the value isn't an object).
fn reshape_dict_entries(v: &Value) -> Vec<Value> {
    let Some(obj) = v.as_object() else {
        return Vec::new();
    };
    obj.iter()
        .map(|(k, val)| match val {
            Value::Object(_) => {
                let mut row = val.clone();
                if let Some(m) = row.as_object_mut() {
                    m.insert("_key".into(), Value::String(k.clone()));
                }
                row
            }
            _ => serde_json::json!({ "_key": k, "_value": val }),
        })
        .collect()
}

/// Flatten a `{ series: [{ …, points: [[t, v], …] }] }` payload into one row per
/// point: the series fields (minus `points`) plus `timestamp`/`value`.
fn reshape_series_points(
    v: &Value,
    series: &str,
    points: &str,
    timestamp: &str,
    value: &str,
) -> Vec<Value> {
    let Some(arr) = v.get(series).and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for s in arr {
        let Some(pts) = s.get(points).and_then(Value::as_array) else {
            continue;
        };
        for pt in pts {
            let Some(pair) = pt.as_array() else { continue };
            let (Some(t), Some(val)) = (pair.first(), pair.get(1)) else {
                continue;
            };
            let mut row = s.clone();
            if let Some(m) = row.as_object_mut() {
                m.remove(points);
                m.insert(timestamp.to_string(), t.clone());
                m.insert(value.to_string(), val.clone());
            }
            out.push(row);
        }
    }
    out
}

fn as_array(v: &Value) -> datafusion::common::Result<Vec<Value>> {
    match v {
        Value::Array(a) => Ok(a.clone()),
        Value::Object(_) => Ok(vec![v.clone()]),
        Value::Null => Ok(Vec::new()),
        other => Err(DataFusionError::Plan(format!(
            "expected array or object at response path, got {other:?}"
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
                            // Integers directly; float-encoded integers (`1.7e12`)
                            // via an f64 fallback (`as` saturates out of range).
                            Some(Value::Number(n)) => {
                                match n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)) {
                                    Some(i) => b.append_value(i),
                                    None => b.append_null(),
                                }
                            }
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
                                match n
                                    .as_i64()
                                    .or_else(|| n.as_f64().map(|f| f as i64))
                                    .and_then(|x| i32::try_from(x).ok())
                                {
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
                            // Numbers that arrive as strings still coerce.
                            Some(Value::String(s)) => match s.trim().parse::<f64>() {
                                Ok(f) => b.append_value(f),
                                Err(_) => b.append_null(),
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
                            // Accept the common stringified forms.
                            Some(Value::String(s)) => {
                                match s.trim().to_ascii_lowercase().as_str() {
                                    "true" => Some(true),
                                    "false" => Some(false),
                                    _ => None,
                                }
                            }
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
    // A bare `YYYY-MM-DD` in a timestamp column is midnight UTC.
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(d.and_hms_opt(0, 0, 0)?.and_utc().timestamp_micros());
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
    if let Some(expr) = &col.expr {
        return expr.eval(row, params);
    }
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

#[cfg(test)]
mod build_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

    use super::*;
    use std::collections::BTreeMap;

    fn base() -> url::Url {
        url::Url::parse("https://api.test/").unwrap()
    }

    #[test]
    fn build_url_excludes_body_params() {
        let mut params = BTreeMap::new();
        params.insert("stream".to_string(), "logs".to_string());
        params.insert("page".to_string(), "2".to_string());
        // `stream` is consumed by the body template, so it must not leak into
        // the query string; `page` (a real query param) stays.
        let url = build_url(
            &base(),
            &[],
            "/_search",
            &params,
            Some("{\"q\": \"{stream}\"}"),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(url.query(), Some("page=2"));
    }

    #[test]
    fn build_url_fills_path_then_query() {
        let mut params = BTreeMap::new();
        params.insert("id".to_string(), "7".to_string());
        params.insert("state".to_string(), "open".to_string());
        let url = build_url(&base(), &[], "/items/{id}", &params, None, &BTreeMap::new()).unwrap();
        assert_eq!(url.path(), "/items/7");
        assert_eq!(url.query(), Some("state=open"));
    }

    #[test]
    fn build_url_explodes_in_list() {
        let mut explode = BTreeMap::new();
        explode.insert("status".to_string(), vec!["a".to_string(), "b".to_string()]);
        let url = build_url(&base(), &[], "/x", &BTreeMap::new(), None, &explode).unwrap();
        assert_eq!(url.query(), Some("status=a&status=b"));
    }

    #[test]
    fn build_batch_evaluates_expr_column() {
        let col = ResponseColumn {
            name: "title".into(),
            r#type: "varchar".into(),
            source: None,
            expr: Some(
                serde_json::from_value(serde_json::json!({
                    "kind": "coalesce",
                    "exprs": [
                        {"kind": "path", "path": ["attributes", "title"]},
                        {"kind": "path", "path": ["title"]}
                    ]
                }))
                .unwrap(),
            ),
        };
        let schema = Arc::new(Schema::new(vec![Field::new("title", DataType::Utf8, true)]));
        let rows = vec![
            serde_json::json!({ "attributes": { "title": "A" } }),
            serde_json::json!({ "title": "B" }),
        ];
        let batch = build_batch(&schema, &[col], &rows, &BTreeMap::new()).unwrap();
        let arr = batch
            .column(0)
            .as_any()
            .downcast_ref::<arrow_array::StringArray>()
            .unwrap();
        assert_eq!(arr.value(0), "A");
        assert_eq!(arr.value(1), "B");
    }

    #[test]
    fn build_batch_coerces_float_number_to_int_columns() {
        // JSON floats that encode integers (`1.7e12`) must populate int columns.
        let cols = vec![
            ResponseColumn {
                name: "ts".into(),
                r#type: "bigint".into(),
                source: None,
                expr: None,
            },
            ResponseColumn {
                name: "small".into(),
                r#type: "int".into(),
                source: None,
                expr: None,
            },
        ];
        let schema = Arc::new(Schema::new(vec![
            Field::new("ts", DataType::Int64, true),
            Field::new("small", DataType::Int32, true),
        ]));
        let rows = vec![serde_json::json!({ "ts": 1_700_000_000_000.0, "small": 42.0 })];
        let batch = build_batch(&schema, &cols, &rows, &BTreeMap::new()).unwrap();
        let ts = batch
            .column(0)
            .as_any()
            .downcast_ref::<arrow_array::Int64Array>()
            .unwrap();
        assert_eq!(ts.value(0), 1_700_000_000_000);
        let small = batch
            .column(1)
            .as_any()
            .downcast_ref::<arrow_array::Int32Array>()
            .unwrap();
        assert_eq!(small.value(0), 42);
    }

    #[test]
    fn as_array_handles_array_object_and_null() {
        // Arrays pass through; a lone object becomes a single row (so endpoints
        // returning one record, e.g. an FX rates object, are modelled directly);
        // null is an empty result.
        assert_eq!(as_array(&serde_json::json!([1, 2])).unwrap().len(), 2);
        let one = as_array(&serde_json::json!({ "base": "USD" })).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0], serde_json::json!({ "base": "USD" }));
        assert!(as_array(&Value::Null).unwrap().is_empty());
        assert!(as_array(&serde_json::json!(42)).is_err());
    }

    #[test]
    fn reshape_dict_entries_keys_to_rows() {
        let v = serde_json::json!({
            "primary": { "background": "#fff", "foreground": "#000" },
            "secondary": { "background": "#eee" }
        });
        let rows = reshape_dict_entries(&v);
        assert_eq!(rows.len(), 2);
        let primary = rows
            .iter()
            .find(|r| r["_key"] == serde_json::json!("primary"));
        assert_eq!(primary.unwrap()["background"], serde_json::json!("#fff"));
    }

    #[test]
    fn reshape_series_points_explodes_pointlist() {
        let v = serde_json::json!({
            "series": [
                { "metric": "cpu", "scope": "host:a", "pointlist": [[1000, 0.5], [2000, 0.7]] }
            ]
        });
        let rows = reshape_series_points(&v, "series", "pointlist", "timestamp", "value");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["metric"], serde_json::json!("cpu"));
        assert_eq!(rows[0]["timestamp"], serde_json::json!(1000));
        assert_eq!(rows[1]["value"], serde_json::json!(0.7));
        assert!(rows[0].get("pointlist").is_none());
    }

    #[test]
    fn extract_in_list_reads_literals() {
        use datafusion::logical_expr::{col, lit};
        let e = col("status").in_list(vec![lit("open"), lit("closed")], false);
        let got = extract_in_list(&e);
        assert_eq!(
            got,
            Some(("status".to_string(), vec!["open".into(), "closed".into()]))
        );
        // Negated IN does not push down.
        let neg = col("status").in_list(vec![lit("open")], true);
        assert_eq!(extract_in_list(&neg), None);
    }

    #[test]
    fn extract_in_list_reads_or_of_equalities() {
        use datafusion::logical_expr::{col, lit};
        // DataFusion lowers a short `IN (...)` to `col = a OR col = b OR col = c`
        // before pushdown; that disjunction must still read as an in-list.
        let e = col("status")
            .eq(lit("open"))
            .or(col("status").eq(lit("closed")))
            .or(col("status").eq(lit("merged")));
        assert_eq!(
            extract_in_list(&e),
            Some((
                "status".to_string(),
                vec!["open".into(), "closed".into(), "merged".into()]
            ))
        );

        // A lone equality is not an in-list (stays an equality push-down).
        assert_eq!(extract_in_list(&col("status").eq(lit("open"))), None);

        // A disjunction spanning two columns is not an in-list.
        let mixed = col("status").eq(lit("open")).or(col("state").eq(lit("x")));
        assert_eq!(extract_in_list(&mixed), None);

        // A disjunction with a non-equality leaf is not an in-list.
        let ranged = col("n").eq(lit(1)).or(col("n").gt(lit(5)));
        assert_eq!(extract_in_list(&ranged), None);
    }

    fn param(name: &str) -> crate::source::ParamSpec {
        serde_json::from_value(serde_json::json!({ "name": name })).unwrap()
    }

    /// A GraphQL-style filter template with one bound and several unbound
    /// optional params: the bound filter survives, the unbound ones (quoted and
    /// unquoted alike) are dropped along with the objects they empty.
    #[test]
    fn render_json_body_prunes_unbound_filters() {
        let template = concat!(
            r#"{"query":"q","variables":{"filter":{"#,
            r#""team":{"id":{"eq":"{team_id}"},"key":{"eq":"{team_key}"}},"#,
            r#""cycle":{"number":{"eq":{cycle_number}}}},"first":100}}"#,
        );
        let declared = vec![param("team_id"), param("team_key"), param("cycle_number")];
        let mut bound = BTreeMap::new();
        bound.insert("team_key".to_string(), "FIN".to_string());

        let out = render_json_body(template, &bound, &declared);
        let v: Value = serde_json::from_str(&out).unwrap();
        // Bound filter kept.
        assert_eq!(v["variables"]["filter"]["team"]["key"]["eq"], "FIN");
        // Unbound `team_id` dropped (its object pruned).
        assert!(v["variables"]["filter"]["team"].get("id").is_none());
        // Unbound unquoted `cycle_number` dropped, emptying `cycle` -> removed.
        assert!(v["variables"]["filter"].get("cycle").is_none());
        // Untouched scalars survive.
        assert_eq!(v["variables"]["first"], 100);
        assert_eq!(v["query"], "q");
    }

    /// When every filter is unbound the whole `filter` object collapses and is
    /// removed, so the GraphQL variable defaults to null (match-all).
    #[test]
    fn render_json_body_drops_fully_unbound_filter_object() {
        let template =
            r#"{"query":"q","variables":{"filter":{"team":{"id":{"eq":"{team_id}"}}},"first":50}}"#;
        let declared = vec![param("team_id")];
        let out = render_json_body(template, &BTreeMap::new(), &declared);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v["variables"].get("filter").is_none());
        assert_eq!(v["variables"]["first"], 50);
    }

    /// A body with no unbound params is returned untouched (fast path).
    #[test]
    fn render_json_body_no_unbound_is_passthrough() {
        let template = r#"{"query":"q","variables":{"first":100}}"#;
        let declared = vec![param("team_id")];
        let mut bound = BTreeMap::new();
        bound.insert("team_id".to_string(), "T".to_string());
        // `team_id` is bound and not in the template; output equals plain render.
        assert_eq!(
            render_json_body(template, &bound, &declared),
            render_template(template, &bound)
        );
    }
}
