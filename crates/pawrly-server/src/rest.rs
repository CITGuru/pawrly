//! JSON-over-HTTP (REST) surface, mounted on the same axum app as gRPC-Web.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
};
use futures_util::StreamExt as _;
use pawrly_core::error::codes;
use pawrly_core::{
    EngineError, EngineService, MaterializeSpec, QueryId, QueryRequest, SemanticQuery, SourceDef,
    TableFilter, TableName, format_batches,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::check_bearer;

/// Default row cap when a request omits `limit`.
const DEFAULT_LIMIT: u64 = 1000;

/// Upper bound on a client-supplied `limit`, so a huge value can't ask the
/// engine to buffer an unbounded result.
const MAX_LIMIT: u64 = 100_000;

/// The hand-maintained OpenAPI 3.0 document, embedded and served at
/// `/v1/openapi.{json,yaml}`. Single source of truth for the REST contract.
const OPENAPI_YAML: &str = include_str!("openapi.yaml");

/// Per-request state shared by every REST handler.
#[derive(Clone)]
struct RestState {
    engine: Arc<dyn EngineService>,
    /// Expected bearer token; `None` disables auth (loopback only).
    bearer: Option<Arc<str>>,
}

/// Build the `/v1/*` + `/healthz` router. State is applied here, so the result
/// is a `Router<()>` ready to `.merge()` into the console app.
pub(crate) fn rest_router(engine: Arc<dyn EngineService>, bearer: Option<Arc<str>>) -> Router {
    Router::new()
        .route("/v1/sql", post(rest_sql))
        .route("/v1/query", post(rest_query))
        .route("/v1/sources", get(rest_sources).post(rest_add_source))
        .route(
            "/v1/sources/:name",
            get(rest_source_detail).delete(rest_remove_source),
        )
        .route("/v1/sources/:name/test", post(rest_test_source))
        .route("/v1/catalog/refresh", post(rest_refresh_catalog))
        .route("/v1/tables", get(rest_tables))
        .route("/v1/tables/:name", get(rest_describe))
        .route("/v1/tables/:name/refresh", post(rest_refresh_table))
        .route("/v1/schema", get(rest_schema))
        .route("/v1/semantic/models", get(rest_semantic_models))
        .route("/v1/semantic/models/:name", get(rest_semantic_model))
        .route("/v1/cache", get(rest_cache))
        .route("/v1/cache/vacuum", post(rest_vacuum_cache))
        .route("/v1/cache/:name", delete(rest_invalidate_cache))
        .route(
            "/v1/materialized/:name",
            put(rest_materialize).delete(rest_drop_materialized),
        )
        .route("/v1/config/reload", post(rest_reload_config))
        .route("/v1/functions", get(rest_functions))
        .route(
            "/v1/functions/:namespace/:name",
            get(rest_describe_function),
        )
        .route("/v1/queries/:id/cancel", post(rest_cancel))
        .route("/v1/explain", post(rest_explain))
        .route("/v1/health", get(rest_health))
        .route("/v1/openapi.json", get(openapi_json))
        .route("/v1/openapi.yaml", get(openapi_yaml))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(RestState { engine, bearer })
}

#[derive(Deserialize)]
struct SqlReq {
    sql: String,
    #[serde(default)]
    params: HashMap<String, String>,
    /// `json` (default) | `ndjson` | `csv`.
    #[serde(default)]
    format: Option<String>,
    /// Row cap; defaults to [`DEFAULT_LIMIT`].
    #[serde(default)]
    limit: Option<u64>,
}

#[derive(Deserialize)]
struct ExplainReq {
    sql: String,
    #[serde(default)]
    analyze: bool,
}

#[derive(Deserialize)]
struct SchemaParams {
    /// Comma-separated source names to scope the snapshot. Absent = all.
    #[serde(default)]
    sources: Option<String>,
    /// Drop per-column detail when true.
    #[serde(default)]
    compact: bool,
}

#[derive(Deserialize)]
struct TablesParams {
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    name_glob: Option<String>,
}

#[derive(Deserialize)]
struct RefreshCatalogParams {
    #[serde(default)]
    source: Option<String>,
}

async fn rest_sql(
    State(state): State<RestState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<SqlReq>,
) -> Response {
    if let Some(resp) = guard(&state, &headers) {
        return resp;
    }
    let limit = req.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    // Ask the engine for one extra row so `format_batches` can report truncation.
    let engine_req = QueryRequest {
        sql: req.sql,
        params: req.params,
        max_rows: limit.saturating_add(1),
        ..Default::default()
    };
    match state.engine.query(engine_req).await {
        Ok(handle) => stream_response(handle.stream, limit, req.format.as_deref()).await,
        Err(e) => engine_error_response(&e),
    }
}

async fn rest_query(
    State(state): State<RestState>,
    headers: axum::http::HeaderMap,
    Json(q): Json<SemanticQuery>,
) -> Response {
    if let Some(resp) = guard(&state, &headers) {
        return resp;
    }
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    match state.engine.semantic_query(q).await {
        Ok(handle) => stream_response(handle.stream, limit, None).await,
        Err(e) => engine_error_response(&e),
    }
}

async fn rest_sources(State(state): State<RestState>, headers: axum::http::HeaderMap) -> Response {
    if let Some(resp) = guard(&state, &headers) {
        return resp;
    }
    match state.engine.list_sources().await {
        Ok(v) => Json(json!({ "sources": v })).into_response(),
        Err(e) => engine_error_response(&e),
    }
}

async fn rest_tables(
    State(state): State<RestState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<TablesParams>,
) -> Response {
    if let Some(resp) = guard(&state, &headers) {
        return resp;
    }
    let filter = (params.source.is_some() || params.name_glob.is_some()).then_some(TableFilter {
        source: params.source,
        name_glob: params.name_glob,
    });
    match state.engine.list_tables(filter).await {
        Ok(v) => Json(json!({ "tables": v })).into_response(),
        Err(e) => engine_error_response(&e),
    }
}

async fn rest_describe(
    State(state): State<RestState>,
    headers: axum::http::HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Some(resp) = guard(&state, &headers) {
        return resp;
    }
    let Some(table) = TableName::parse(&name) else {
        return error_response(
            StatusCode::BAD_REQUEST,
            codes::INVALID_SQL,
            &format!("expected `schema.table`, got `{name}`"),
        );
    };
    match state.engine.describe_table(&table).await {
        Ok(desc) => Json(desc).into_response(),
        Err(e) => engine_error_response(&e),
    }
}

async fn rest_explain(
    State(state): State<RestState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ExplainReq>,
) -> Response {
    if let Some(resp) = guard(&state, &headers) {
        return resp;
    }
    match state.engine.explain(&req.sql, req.analyze).await {
        Ok(plan) => Json(json!({ "plan": plan })).into_response(),
        Err(e) => engine_error_response(&e),
    }
}

async fn rest_source_detail(
    State(state): State<RestState>,
    headers: axum::http::HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Some(resp) = guard(&state, &headers) {
        return resp;
    }
    match state.engine.list_sources().await {
        Ok(sources) => match sources.into_iter().find(|s| s.name == name) {
            Some(src) => Json(src).into_response(),
            None => error_response(
                StatusCode::NOT_FOUND,
                codes::UNKNOWN_SOURCE,
                &format!("no source named `{name}`"),
            ),
        },
        Err(e) => engine_error_response(&e),
    }
}

async fn rest_schema(
    State(state): State<RestState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<SchemaParams>,
) -> Response {
    if let Some(resp) = guard(&state, &headers) {
        return resp;
    }
    let sources = params.sources.as_deref().map(|s| {
        s.split(',')
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(String::from)
            .collect::<Vec<_>>()
    });
    match state.engine.schema_snapshot(sources, params.compact).await {
        Ok(snap) => Json(snap).into_response(),
        Err(e) => engine_error_response(&e),
    }
}

async fn rest_semantic_models(
    State(state): State<RestState>,
    headers: axum::http::HeaderMap,
) -> Response {
    if let Some(resp) = guard(&state, &headers) {
        return resp;
    }
    match state.engine.list_semantic_models().await {
        Ok(v) => Json(json!({ "models": v })).into_response(),
        Err(e) => engine_error_response(&e),
    }
}

async fn rest_semantic_model(
    State(state): State<RestState>,
    headers: axum::http::HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Some(resp) = guard(&state, &headers) {
        return resp;
    }
    match state.engine.describe_semantic_model(&name).await {
        Ok(model) => Json(model).into_response(),
        Err(e) => engine_error_response(&e),
    }
}

/// `?namespace=`; absent/empty = the default workspace namespace.
#[derive(Deserialize)]
struct NamespaceParams {
    namespace: Option<String>,
}

impl NamespaceParams {
    fn namespace(&self) -> Option<&str> {
        self.namespace.as_deref().filter(|ns| !ns.is_empty())
    }
}

async fn rest_cache(
    State(state): State<RestState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<NamespaceParams>,
) -> Response {
    if let Some(resp) = guard(&state, &headers) {
        return resp;
    }
    match state.engine.cache_entries(params.namespace()).await {
        Ok(v) => Json(with_namespace_echo(json!({ "entries": v }), &params)).into_response(),
        Err(e) => engine_error_response(&e),
    }
}

/// The echo is how a client detects a server that ignored its namespace;
/// absent when none was requested, so pre-namespace response shapes hold.
fn with_namespace_echo(mut body: serde_json::Value, params: &NamespaceParams) -> serde_json::Value {
    if let Some(ns) = params.namespace() {
        body["namespace"] = json!(ns);
    }
    body
}

async fn rest_health(State(state): State<RestState>, headers: axum::http::HeaderMap) -> Response {
    if let Some(resp) = guard(&state, &headers) {
        return resp;
    }
    match state.engine.health().await {
        Ok(h) => Json(h).into_response(),
        Err(e) => engine_error_response(&e),
    }
}

/// Create or replace the materialized table `name`. Body is a [`MaterializeSpec`]
/// (`{"kind":"query","sql":"…"}`, `{"kind":"file","path":"…"}`, `url`, or
/// `inline`). `Json` is last so it consumes the request body after `Path`.
async fn rest_materialize(
    State(state): State<RestState>,
    headers: axum::http::HeaderMap,
    Path(name): Path<String>,
    Query(params): Query<NamespaceParams>,
    Json(spec): Json<MaterializeSpec>,
) -> Response {
    if let Some(resp) = guard(&state, &headers) {
        return resp;
    }
    match state
        .engine
        .materialize(&name, spec, params.namespace())
        .await
    {
        Ok(outcome) => Json(with_namespace_echo(json!(outcome), &params)).into_response(),
        Err(e) => engine_error_response(&e),
    }
}

async fn rest_drop_materialized(
    State(state): State<RestState>,
    headers: axum::http::HeaderMap,
    Path(name): Path<String>,
    Query(params): Query<NamespaceParams>,
) -> Response {
    if let Some(resp) = guard(&state, &headers) {
        return resp;
    }
    match state
        .engine
        .drop_materialized(&name, params.namespace())
        .await
    {
        Ok(true) => Json(with_namespace_echo(
            json!({ "dropped": true, "name": name }),
            &params,
        ))
        .into_response(),
        Ok(false) => error_response(
            StatusCode::NOT_FOUND,
            codes::UNKNOWN_MATERIALIZED,
            &format!("no materialized table named `{name}`"),
        ),
        Err(e) => engine_error_response(&e),
    }
}

/// Serve the embedded OpenAPI document as JSON (unauthenticated, like `/healthz`).
async fn openapi_json() -> Response {
    match serde_yaml::from_str::<serde_json::Value>(OPENAPI_YAML) {
        Ok(doc) => Json(doc).into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            codes::INTERNAL,
            &format!("openapi spec: {e}"),
        ),
    }
}

/// Serve the embedded OpenAPI document as raw YAML.
async fn openapi_yaml() -> Response {
    ([(header::CONTENT_TYPE, "application/yaml")], OPENAPI_YAML).into_response()
}

/// Bearer gate. `None` to proceed; `Some(resp)` is a ready `401` to return.
fn guard(state: &RestState, headers: &axum::http::HeaderMap) -> Option<Response> {
    if check_bearer(state.bearer.as_deref(), headers) {
        None
    } else {
        Some(error_response(
            StatusCode::UNAUTHORIZED,
            codes::UNAUTHORIZED,
            "missing or invalid bearer token",
        ))
    }
}

/// Encode the result stream in the requested format. `ndjson` streams as batches
/// arrive (`Body::from_stream`); `json`/`csv` buffer, since the JSON envelope
/// needs the terminal row count.
async fn stream_response(
    mut stream: pawrly_core::QueryStream,
    limit: u64,
    format: Option<&str>,
) -> Response {
    let fmt = format.unwrap_or("json");
    match fmt {
        "ndjson" => {
            let body = Body::from_stream(ndjson_stream(stream, limit as usize));
            return ([(header::CONTENT_TYPE, "application/x-ndjson")], body).into_response();
        }
        "json" | "csv" => {}
        other => {
            return error_response(
                StatusCode::BAD_REQUEST,
                codes::BAD_FORMAT,
                &format!("unknown format `{other}`; use json | ndjson | csv"),
            );
        }
    }

    let mut batches = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(batch) => batches.push(batch),
            Err(e) => return engine_error_response(&e),
        }
    }
    let (columns, rows, total, truncated) = format_batches(&batches, limit as usize);

    if fmt == "csv" {
        return (
            [(header::CONTENT_TYPE, "text/csv")],
            rows_to_csv(&columns, &rows),
        )
            .into_response();
    }
    let objects: Vec<Value> = rows.iter().map(|r| row_object(&columns, r)).collect();
    Json(json!({
        "columns": columns,
        "rows": objects,
        "row_count": total,
        "truncated": truncated,
    }))
    .into_response()
}

/// Stream a result as NDJSON — one row-object per line — capped at `limit` rows,
/// encoding and flushing each batch as it arrives rather than buffering the
/// whole result.
fn ndjson_stream(
    mut stream: pawrly_core::QueryStream,
    limit: usize,
) -> impl futures_util::Stream<Item = Result<String, std::io::Error>> {
    async_stream::stream! {
        let mut remaining = limit;
        while remaining > 0 {
            match stream.next().await {
                Some(Ok(batch)) => {
                    let (columns, rows, total, _) = format_batches(&[batch], remaining);
                    remaining -= total;
                    let mut buf = String::new();
                    for row in &rows {
                        buf.push_str(&row_object(&columns, row).to_string());
                        buf.push('\n');
                    }
                    if !buf.is_empty() {
                        yield Ok(buf);
                    }
                }
                Some(Err(e)) => {
                    yield Err(std::io::Error::other(e.to_string()));
                    return;
                }
                None => break,
            }
        }
    }
}

/// Zip a positional row with its column names into a JSON object.
fn row_object(columns: &[String], row: &[Value]) -> Value {
    let mut map = serde_json::Map::with_capacity(columns.len());
    for (col, val) in columns.iter().zip(row) {
        map.insert(col.clone(), val.clone());
    }
    Value::Object(map)
}

/// Render columns + rows as RFC 4180 CSV; cells are the typed JSON scalars from
/// `format_batches`.
fn rows_to_csv(columns: &[String], rows: &[Vec<Value>]) -> String {
    let mut out = String::new();
    push_csv_row(&mut out, columns.iter().map(String::as_str));
    for row in rows {
        let cells: Vec<String> = row.iter().map(csv_cell).collect();
        push_csv_row(&mut out, cells.iter().map(String::as_str));
    }
    out
}

fn csv_cell(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn push_csv_row<'a>(out: &mut String, cells: impl Iterator<Item = &'a str>) {
    let mut first = true;
    for cell in cells {
        if !first {
            out.push(',');
        }
        first = false;
        if cell.contains(['"', ',', '\n', '\r']) {
            out.push('"');
            out.push_str(&cell.replace('"', "\"\""));
            out.push('"');
        } else {
            out.push_str(cell);
        }
    }
    out.push('\n');
}

async fn rest_add_source(
    State(state): State<RestState>,
    headers: axum::http::HeaderMap,
    Json(def): Json<SourceDef>,
) -> Response {
    if let Some(resp) = guard(&state, &headers) {
        return resp;
    }
    match state.engine.add_source(def).await {
        Ok(info) => Json(info).into_response(),
        Err(e) => engine_error_response(&e),
    }
}

async fn rest_remove_source(
    State(state): State<RestState>,
    headers: axum::http::HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Some(resp) = guard(&state, &headers) {
        return resp;
    }
    match state.engine.remove_source(&name).await {
        Ok(true) => Json(json!({ "removed": true, "name": name })).into_response(),
        Ok(false) => error_response(
            StatusCode::NOT_FOUND,
            codes::UNKNOWN_SOURCE,
            &format!("no source named `{name}`"),
        ),
        Err(e) => engine_error_response(&e),
    }
}

async fn rest_test_source(
    State(state): State<RestState>,
    headers: axum::http::HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Some(resp) = guard(&state, &headers) {
        return resp;
    }
    match state.engine.test_source(&name).await {
        Ok(report) => Json(report).into_response(),
        Err(e) => engine_error_response(&e),
    }
}

async fn rest_refresh_catalog(
    State(state): State<RestState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<RefreshCatalogParams>,
) -> Response {
    if let Some(resp) = guard(&state, &headers) {
        return resp;
    }
    match state.engine.refresh_catalog(params.source.as_deref()).await {
        Ok(outcome) => Json(outcome).into_response(),
        Err(e) => engine_error_response(&e),
    }
}

async fn rest_refresh_table(
    State(state): State<RestState>,
    headers: axum::http::HeaderMap,
    Path(name): Path<String>,
    Query(params): Query<NamespaceParams>,
) -> Response {
    if let Some(resp) = guard(&state, &headers) {
        return resp;
    }
    let Some(table) = TableName::parse(&name) else {
        return error_response(
            StatusCode::BAD_REQUEST,
            codes::INVALID_SQL,
            &format!("expected `schema.table`, got `{name}`"),
        );
    };
    match state.engine.refresh_table(&table, params.namespace()).await {
        Ok(outcome) => Json(with_namespace_echo(json!(outcome), &params)).into_response(),
        Err(e) => engine_error_response(&e),
    }
}

async fn rest_vacuum_cache(
    State(state): State<RestState>,
    headers: axum::http::HeaderMap,
) -> Response {
    if let Some(resp) = guard(&state, &headers) {
        return resp;
    }
    match state.engine.vacuum_cache().await {
        Ok(report) => Json(report).into_response(),
        Err(e) => engine_error_response(&e),
    }
}

async fn rest_invalidate_cache(
    State(state): State<RestState>,
    headers: axum::http::HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Some(resp) = guard(&state, &headers) {
        return resp;
    }
    let Some(table) = TableName::parse(&name) else {
        return error_response(
            StatusCode::BAD_REQUEST,
            codes::INVALID_SQL,
            &format!("expected `schema.table`, got `{name}`"),
        );
    };
    match state.engine.invalidate_cache(&table).await {
        Ok(invalidated) => {
            Json(json!({ "invalidated": invalidated, "name": name })).into_response()
        }
        Err(e) => engine_error_response(&e),
    }
}

async fn rest_reload_config(
    State(state): State<RestState>,
    headers: axum::http::HeaderMap,
) -> Response {
    if let Some(resp) = guard(&state, &headers) {
        return resp;
    }
    match state.engine.reload_config().await {
        Ok(report) => Json(report).into_response(),
        Err(e) => engine_error_response(&e),
    }
}

async fn rest_functions(
    State(state): State<RestState>,
    headers: axum::http::HeaderMap,
) -> Response {
    if let Some(resp) = guard(&state, &headers) {
        return resp;
    }
    match state.engine.list_functions().await {
        Ok(v) => Json(json!({ "functions": v })).into_response(),
        Err(e) => engine_error_response(&e),
    }
}

async fn rest_describe_function(
    State(state): State<RestState>,
    headers: axum::http::HeaderMap,
    Path((namespace, name)): Path<(String, String)>,
) -> Response {
    if let Some(resp) = guard(&state, &headers) {
        return resp;
    }
    match state.engine.describe_function(&namespace, &name).await {
        Ok(desc) => Json(desc).into_response(),
        Err(e) => engine_error_response(&e),
    }
}

async fn rest_cancel(
    State(state): State<RestState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Some(resp) = guard(&state, &headers) {
        return resp;
    }
    match state.engine.cancel(&QueryId::new(id)).await {
        Ok(cancelled) => Json(json!({ "cancelled": cancelled })).into_response(),
        Err(e) => engine_error_response(&e),
    }
}

/// Map an [`EngineError`] to an HTTP status, preserving its stable `PAWRLY_*`
/// code. Mirrors the gRPC `engine_error_to_status` categorisation.
fn engine_error_response(err: &EngineError) -> Response {
    let status = match err {
        EngineError::UnknownTable(_) | EngineError::UnknownFunction(_) => StatusCode::NOT_FOUND,
        EngineError::UnknownKind(_)
        | EngineError::InvalidSql(_)
        | EngineError::SemanticPlan(_)
        | EngineError::Safety(_)
        | EngineError::SourceRegistration { .. } => StatusCode::BAD_REQUEST,
        EngineError::Timeout(_) => StatusCode::REQUEST_TIMEOUT,
        EngineError::OutOfMemory(_) => StatusCode::SERVICE_UNAVAILABLE,
        EngineError::Cancelled => {
            StatusCode::from_u16(499).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
        }
        EngineError::Protocol(_) | EngineError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        EngineError::Unsupported(_) => StatusCode::NOT_IMPLEMENTED,
    };
    error_response(status, err.code(), &err.to_string())
}

/// A JSON error envelope: `{ "error": { "code", "message" } }`.
fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({ "error": { "code": code, "message": message } })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use pawrly_core::SourceKind;
    use pawrly_core::test_support::MockEngine;
    use tower::ServiceExt as _;

    fn app(engine: MockEngine, bearer: Option<&str>) -> Router {
        rest_router(Arc::new(engine), bearer.map(Arc::from))
    }

    async fn json_body(resp: Response) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn post_json(uri: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn sql_returns_rows_as_objects() {
        let engine = MockEngine::new();
        engine.canned("SELECT", vec![MockEngine::one_row(1, "a")]);
        let resp = app(engine, None)
            .oneshot(post_json("/v1/sql", r#"{"sql":"SELECT 1"}"#))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = json_body(resp).await;
        assert_eq!(v["row_count"], 1);
        // Integer columns come back as JSON numbers, not stringified.
        assert_eq!(v["rows"][0]["id"], 1);
        assert!(v["rows"][0]["id"].is_number());
        assert_eq!(v["rows"][0]["label"], "a");
    }

    #[tokio::test]
    async fn sql_ndjson_format() {
        let engine = MockEngine::new();
        engine.canned("SELECT", vec![MockEngine::one_row(7, "z")]);
        let resp = app(engine, None)
            .oneshot(post_json(
                "/v1/sql",
                r#"{"sql":"SELECT 1","format":"ndjson"}"#,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(ct.contains("application/x-ndjson"), "content-type: {ct}");
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let line: Value =
            serde_json::from_slice(bytes.split(|b| *b == b'\n').next().unwrap()).unwrap();
        assert_eq!(line["id"], 7);
        assert!(line["id"].is_number());
    }

    #[tokio::test]
    async fn ndjson_streams_and_caps_at_limit() {
        let engine = MockEngine::new();
        engine.canned(
            "SELECT",
            vec![
                MockEngine::one_row(1, "a"),
                MockEngine::one_row(2, "b"),
                MockEngine::one_row(3, "c"),
            ],
        );
        let resp = app(engine, None)
            .oneshot(post_json(
                "/v1/sql",
                r#"{"sql":"SELECT 1","format":"ndjson","limit":2}"#,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(ct.contains("application/x-ndjson"), "content-type: {ct}");
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 2, "limit=2 should cap the stream: {text}");
        let first: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["id"], 1);
        assert!(first["id"].is_number());
    }

    #[test]
    fn csv_renders_typed_scalars() {
        // With typed cells, CSV must still render numbers/bools as text
        // (not blank) and null as empty.
        let cols = vec!["n".to_string(), "b".to_string(), "s".to_string()];
        let rows = vec![
            vec![Value::from(42), Value::Bool(true), Value::from("hi")],
            vec![Value::Null, Value::Null, Value::Null],
        ];
        let csv = rows_to_csv(&cols, &rows);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines[0], "n,b,s");
        assert_eq!(lines[1], "42,true,hi");
        assert_eq!(lines[2], ",,");
    }

    fn get_req(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    fn delete_req(uri: &str) -> Request<Body> {
        Request::builder()
            .method("DELETE")
            .uri(uri)
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn functions_list_ok() {
        let resp = app(MockEngine::new(), None)
            .oneshot(get_req("/v1/functions"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(json_body(resp).await["functions"].is_array());
    }

    #[tokio::test]
    async fn functions_requires_bearer() {
        let resp = app(MockEngine::new(), Some("s3cret"))
            .oneshot(get_req("/v1/functions"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn cancel_reports_false_when_nothing_in_flight() {
        let resp = app(MockEngine::new(), None)
            .oneshot(post_json("/v1/queries/abc/cancel", ""))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(json_body(resp).await["cancelled"], false);
    }

    #[tokio::test]
    async fn invalidate_bad_name_is_400() {
        let resp = app(MockEngine::new(), None)
            .oneshot(delete_req("/v1/cache/notaname"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(json_body(resp).await["error"]["code"], "PAWRLY_INVALID_SQL");
    }

    #[tokio::test]
    async fn tables_accepts_filter_params() {
        let resp = app(MockEngine::new(), None)
            .oneshot(get_req("/v1/tables?source=gh&name_glob=iss*"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(json_body(resp).await["tables"].is_array());
    }

    #[tokio::test]
    async fn missing_bearer_is_unauthorized() {
        let resp = app(MockEngine::new(), Some("s3cret"))
            .oneshot(post_json("/v1/sql", r#"{"sql":"SELECT 1"}"#))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let v = json_body(resp).await;
        assert_eq!(v["error"]["code"], "PAWRLY_UNAUTHORIZED");
    }

    #[tokio::test]
    async fn valid_bearer_passes() {
        let engine = MockEngine::new();
        engine.canned("SELECT", vec![MockEngine::one_row(1, "a")]);
        let req = Request::builder()
            .method("POST")
            .uri("/v1/sql")
            .header("content-type", "application/json")
            .header("authorization", "Bearer s3cret")
            .body(Body::from(r#"{"sql":"SELECT 1"}"#.to_string()))
            .unwrap();
        let resp = app(engine, Some("s3cret")).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn healthz_needs_no_auth() {
        let resp = app(MockEngine::new(), Some("s3cret"))
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn sources_listed() {
        let engine = MockEngine::new();
        engine.add_source("gh", SourceKind::Http);
        let resp = app(engine, None)
            .oneshot(
                Request::builder()
                    .uri("/v1/sources")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = json_body(resp).await;
        assert!(
            v["sources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["name"] == "gh"),
            "sources: {v}"
        );
    }

    #[tokio::test]
    async fn describe_unknown_table_is_404() {
        let resp = app(MockEngine::new(), None)
            .oneshot(
                Request::builder()
                    .uri("/v1/tables/foo.bar")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let v = json_body(resp).await;
        assert_eq!(v["error"]["code"], "PAWRLY_UNKNOWN_TABLE");
    }

    async fn get(engine: MockEngine, uri: &str) -> Response {
        app(engine, None)
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn source_detail_found_and_missing() {
        let engine = MockEngine::new();
        engine.add_source("gh", SourceKind::Http);
        let found = get(engine, "/v1/sources/gh").await;
        assert_eq!(found.status(), StatusCode::OK);
        assert_eq!(json_body(found).await["name"], "gh");

        let engine = MockEngine::new();
        engine.add_source("gh", SourceKind::Http);
        let missing = get(engine, "/v1/sources/nope").await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            json_body(missing).await["error"]["code"],
            "PAWRLY_UNKNOWN_SOURCE"
        );
    }

    #[tokio::test]
    async fn schema_models_cache_health_are_200() {
        assert_eq!(
            get(MockEngine::new(), "/v1/schema").await.status(),
            StatusCode::OK
        );
        assert_eq!(
            get(MockEngine::new(), "/v1/health").await.status(),
            StatusCode::OK
        );

        let models = get(MockEngine::new(), "/v1/semantic/models").await;
        assert_eq!(models.status(), StatusCode::OK);
        assert!(json_body(models).await["models"].is_array());

        let cache = get(MockEngine::new(), "/v1/cache").await;
        assert_eq!(cache.status(), StatusCode::OK);
        assert!(json_body(cache).await["entries"].is_array());
    }

    fn put_materialized(name: &str, spec: &str, bearer: Option<&str>) -> Request<Body> {
        let mut b = Request::builder()
            .method("PUT")
            .uri(format!("/v1/materialized/{name}"))
            .header("content-type", "application/json");
        if let Some(t) = bearer {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        b.body(Body::from(spec.to_string())).unwrap()
    }

    #[tokio::test]
    async fn materialize_put_returns_outcome() {
        let resp = app(MockEngine::new(), None)
            .oneshot(put_materialized(
                "daily",
                r#"{"kind":"query","sql":"SELECT 1"}"#,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(json_body(resp).await["row_count"], 0);
    }

    #[tokio::test]
    async fn materialize_requires_bearer() {
        let resp = app(MockEngine::new(), Some("s3cret"))
            .oneshot(put_materialized(
                "daily",
                r#"{"kind":"query","sql":"SELECT 1"}"#,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn drop_materialized_missing_is_404() {
        let resp = app(MockEngine::new(), None)
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/v1/materialized/daily")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            json_body(resp).await["error"]["code"],
            "PAWRLY_UNKNOWN_MATERIALIZED"
        );
    }

    #[test]
    fn openapi_spec_parses_and_covers_routes() {
        let doc: serde_json::Value = serde_yaml::from_str(OPENAPI_YAML).unwrap();
        assert_eq!(doc["openapi"], "3.0.3");
        // Cross-check the spec against the routes actually registered, so the
        // two can't drift. OpenAPI uses `{name}`; routes use `:name`.
        for path in [
            "/v1/sql",
            "/v1/query",
            "/v1/explain",
            "/v1/sources",
            "/v1/sources/{name}",
            "/v1/tables",
            "/v1/tables/{name}",
            "/v1/schema",
            "/v1/semantic/models",
            "/v1/semantic/models/{name}",
            "/v1/cache",
            "/v1/cache/vacuum",
            "/v1/cache/{name}",
            "/v1/materialized/{name}",
            "/v1/sources/{name}/test",
            "/v1/catalog/refresh",
            "/v1/tables/{name}/refresh",
            "/v1/config/reload",
            "/v1/functions",
            "/v1/functions/{namespace}/{name}",
            "/v1/queries/{id}/cancel",
            "/v1/health",
            "/healthz",
        ] {
            assert!(
                doc["paths"][path].is_object(),
                "openapi.yaml missing path {path}"
            );
        }
    }

    #[tokio::test]
    async fn openapi_json_is_public_even_with_bearer() {
        let resp = app(MockEngine::new(), Some("s3cret"))
            .oneshot(
                Request::builder()
                    .uri("/v1/openapi.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = json_body(resp).await;
        assert!(v["paths"]["/v1/sql"]["post"].is_object());
    }
}
