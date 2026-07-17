//! `RestEngineClient` — an `EngineService` implementation over the JSON REST
//! surface (`pawrly console` / `serve --console`).
//!
//! Every method maps to a `/v1/*` route. Results deserialize straight into the
//! core types; `query`/`semantic_query` rebuild Arrow `RecordBatch`es from the
//! JSON rows via type inference (REST has no typed Arrow wire), so values are
//! best-effort typed, not lossless. `shutdown` is [`EngineError::Unsupported`]
//! — a daemon won't stop itself for a client.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use arrow_array::builder::{BooleanBuilder, Float64Builder, Int64Builder, StringBuilder};
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use async_trait::async_trait;
use pawrly_core::semantic::{SemanticModelDescription, SemanticModelInfo, SemanticQuery};
use pawrly_core::{
    CacheEntryInfo, CatalogSnapshot, EngineError, EngineService, FunctionDescription, FunctionInfo,
    HealthReport, MaterializeOutcome, MaterializeSpec, QueryCompleted, QueryHandle, QueryId,
    QueryRequest, RefreshCatalogOutcome, RefreshOutcome, ReloadReport, SourceDef, SourceInfo,
    SourceTestReport, TableDescription, TableFilter, TableInfo, TableName, VacuumReport,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// `EngineService` over the REST/JSON transport.
pub struct RestEngineClient {
    http: reqwest::Client,
    /// Base URL without a trailing slash, e.g. `http://127.0.0.1:8787`.
    base: String,
    bearer: Option<String>,
}

impl RestEngineClient {
    /// Build a client for the console/REST bind at `base_url`.
    #[must_use]
    pub fn new(base_url: impl Into<String>, bearer: Option<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: base_url.into().trim_end_matches('/').to_string(),
            bearer,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Send a request (attaching the bearer), then parse the JSON body. A
    /// non-2xx status is turned into an `EngineError` from the error envelope.
    async fn send(&self, rb: reqwest::RequestBuilder) -> Result<Value, EngineError> {
        let rb = match &self.bearer {
            Some(t) => rb.bearer_auth(t),
            None => rb,
        };
        let resp = rb
            .send()
            .await
            .map_err(|e| EngineError::Protocol(format!("rest request: {e}")))?;
        let status = resp.status();
        let body: Value = resp
            .json()
            .await
            .map_err(|e| EngineError::Protocol(format!("rest decode: {e}")))?;
        if status.is_success() {
            Ok(body)
        } else {
            Err(rest_error(&body))
        }
    }

    async fn get(&self, path: &str) -> Result<Value, EngineError> {
        self.send(self.http.get(self.url(path))).await
    }

    async fn get_query(&self, path: &str, params: &[(&str, String)]) -> Result<Value, EngineError> {
        self.send(self.http.get(self.url(path)).query(params)).await
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value, EngineError> {
        self.send(self.http.post(self.url(path)).json(&body)).await
    }

    async fn post_empty(&self, path: &str) -> Result<Value, EngineError> {
        self.send(self.http.post(self.url(path))).await
    }

    async fn delete(&self, path: &str) -> Result<Value, EngineError> {
        self.send(self.http.delete(self.url(path))).await
    }
}

/// Turn a `{ "error": { code, message } }` envelope into an `EngineError`.
fn rest_error(body: &Value) -> EngineError {
    let code = body["error"]["code"].as_str().unwrap_or("PAWRLY_INTERNAL");
    let msg = body["error"]["message"].as_str().unwrap_or("");
    EngineError::from_wire(code, msg)
}

fn require_namespace_echo(requested: Option<&str>, resp: &Value) -> Result<(), EngineError> {
    match requested {
        Some(ns) if !ns.is_empty() && resp.get("namespace").and_then(Value::as_str) != Some(ns) => {
            Err(EngineError::Protocol(format!(
                "server ignored namespace `{ns}` — it predates materialize namespaces, so the \
                 operation targeted the default namespace instead; upgrade the server"
            )))
        }
        _ => Ok(()),
    }
}

fn field<T: DeserializeOwned>(body: &Value, key: &str) -> Result<T, EngineError> {
    serde_json::from_value(body.get(key).cloned().unwrap_or(Value::Null))
        .map_err(|e| EngineError::Protocol(format!("rest decode `{key}`: {e}")))
}

fn whole<T: DeserializeOwned>(body: Value) -> Result<T, EngineError> {
    serde_json::from_value(body).map_err(|e| EngineError::Protocol(format!("rest decode: {e}")))
}

fn unsupported(method: &str) -> EngineError {
    EngineError::Unsupported(format!(
        "`{method}` is not supported over the REST transport"
    ))
}

/// Build a `QueryHandle` from a `/v1/sql`-shaped JSON envelope
/// (`{ columns, rows, row_count, truncated }`), rebuilding a single Arrow batch.
fn json_to_handle(body: &Value) -> Result<QueryHandle, EngineError> {
    let columns: Vec<String> = body["columns"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let rows = body["rows"].as_array().cloned().unwrap_or_default();
    let batch = json_rows_to_batch(&columns, &rows)?;

    let completion = Arc::new(OnceLock::new());
    let _ = completion.set(QueryCompleted {
        rows_returned: body["row_count"].as_u64().unwrap_or(rows.len() as u64),
        truncated: body["truncated"].as_bool().unwrap_or(false),
        elapsed: Duration::ZERO,
    });
    let stream = futures_util::stream::iter(std::iter::once(Ok(batch)));
    Ok(QueryHandle::new(
        QueryId::new(String::new()),
        Box::pin(stream),
        completion,
    ))
}

/// Rebuild a single `RecordBatch` from JSON row-objects, inferring each column's
/// Arrow type from its values (int/float/bool typed; mixed or anything else as
/// strings; all-null columns default to `Utf8`).
fn json_rows_to_batch(columns: &[String], rows: &[Value]) -> Result<RecordBatch, EngineError> {
    if columns.is_empty() {
        return RecordBatch::try_new(Arc::new(Schema::empty()), Vec::new())
            .map_err(|e| EngineError::Protocol(format!("rest query: {e}")));
    }
    let mut fields = Vec::with_capacity(columns.len());
    let mut arrays: Vec<ArrayRef> = Vec::with_capacity(columns.len());
    for col in columns {
        let vals: Vec<&Value> = rows.iter().map(|r| &r[col.as_str()]).collect();
        let (dt, array) = build_column(&vals);
        fields.push(Field::new(col, dt, true));
        arrays.push(array);
    }
    RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays)
        .map_err(|e| EngineError::Protocol(format!("rest query: {e}")))
}

fn build_column(vals: &[&Value]) -> (DataType, ArrayRef) {
    let (mut has_float, mut has_int, mut has_bool, mut has_str) = (false, false, false, false);
    for v in vals {
        match v {
            Value::Null => {}
            Value::Number(n) if n.is_i64() || n.is_u64() => has_int = true,
            Value::Number(_) => has_float = true,
            Value::Bool(_) => has_bool = true,
            _ => has_str = true,
        }
    }
    // A string, or bools mixed with numbers, collapse to a stringified column.
    if has_str || (has_bool && (has_int || has_float)) {
        let mut b = StringBuilder::new();
        for v in vals {
            match v {
                Value::Null => b.append_null(),
                Value::String(s) => b.append_value(s),
                other => b.append_value(other.to_string()),
            }
        }
        (DataType::Utf8, Arc::new(b.finish()))
    } else if has_float {
        let mut b = Float64Builder::new();
        for v in vals {
            b.append_option(v.as_f64());
        }
        (DataType::Float64, Arc::new(b.finish()))
    } else if has_bool {
        let mut b = BooleanBuilder::new();
        for v in vals {
            b.append_option(v.as_bool());
        }
        (DataType::Boolean, Arc::new(b.finish()))
    } else if has_int {
        let mut b = Int64Builder::new();
        for v in vals {
            b.append_option(v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)));
        }
        (DataType::Int64, Arc::new(b.finish()))
    } else {
        let mut b = StringBuilder::new();
        for _ in vals {
            b.append_null();
        }
        (DataType::Utf8, Arc::new(b.finish()))
    }
}

#[async_trait]
impl EngineService for RestEngineClient {
    async fn query(&self, req: QueryRequest) -> Result<QueryHandle, EngineError> {
        let mut body = json!({ "sql": req.sql, "params": req.params });
        if req.max_rows > 0 {
            body["limit"] = req.max_rows.into();
        }
        let resp = self.post("/v1/sql", body).await?;
        json_to_handle(&resp)
    }

    async fn explain(&self, sql: &str, analyze: bool) -> Result<String, EngineError> {
        let resp = self
            .post("/v1/explain", json!({ "sql": sql, "analyze": analyze }))
            .await?;
        field(&resp, "plan")
    }

    async fn cancel(&self, query_id: &QueryId) -> Result<bool, EngineError> {
        let resp = self
            .post_empty(&format!("/v1/queries/{}/cancel", query_id.0))
            .await?;
        field(&resp, "cancelled")
    }

    async fn list_sources(&self) -> Result<Vec<SourceInfo>, EngineError> {
        let resp = self.get("/v1/sources").await?;
        field(&resp, "sources")
    }

    async fn list_tables(
        &self,
        filter: Option<TableFilter>,
    ) -> Result<Vec<TableInfo>, EngineError> {
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(f) = filter {
            if let Some(s) = f.source {
                params.push(("source", s));
            }
            if let Some(g) = f.name_glob {
                params.push(("name_glob", g));
            }
        }
        let resp = self.get_query("/v1/tables", &params).await?;
        field(&resp, "tables")
    }

    async fn describe_table(&self, name: &TableName) -> Result<TableDescription, EngineError> {
        whole(self.get(&format!("/v1/tables/{name}")).await?)
    }

    async fn schema_snapshot(
        &self,
        sources: Option<Vec<String>>,
        compact: bool,
    ) -> Result<CatalogSnapshot, EngineError> {
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(s) = sources {
            params.push(("sources", s.join(",")));
        }
        if compact {
            params.push(("compact", "true".into()));
        }
        whole(self.get_query("/v1/schema", &params).await?)
    }

    async fn refresh_catalog(
        &self,
        source: Option<&str>,
    ) -> Result<RefreshCatalogOutcome, EngineError> {
        let params: Vec<(&str, String)> = source
            .map(|s| vec![("source", s.to_string())])
            .unwrap_or_default();
        whole(
            self.send(
                self.http
                    .post(self.url("/v1/catalog/refresh"))
                    .query(&params),
            )
            .await?,
        )
    }

    async fn cache_entries(
        &self,
        namespace: Option<&str>,
    ) -> Result<Vec<CacheEntryInfo>, EngineError> {
        let resp = match namespace {
            Some(ns) => {
                self.get_query("/v1/cache", &[("namespace", ns.to_string())])
                    .await?
            }
            None => self.get("/v1/cache").await?,
        };
        require_namespace_echo(namespace, &resp)?;
        field(&resp, "entries")
    }

    async fn refresh_table(&self, name: &TableName) -> Result<RefreshOutcome, EngineError> {
        whole(
            self.post_empty(&format!("/v1/tables/{name}/refresh"))
                .await?,
        )
    }

    async fn invalidate_cache(&self, name: &TableName) -> Result<bool, EngineError> {
        let resp = self.delete(&format!("/v1/cache/{name}")).await?;
        field(&resp, "invalidated")
    }

    async fn vacuum_cache(&self) -> Result<VacuumReport, EngineError> {
        whole(self.post_empty("/v1/cache/vacuum").await?)
    }

    async fn materialize(
        &self,
        name: &str,
        spec: MaterializeSpec,
        namespace: Option<&str>,
    ) -> Result<MaterializeOutcome, EngineError> {
        let body = serde_json::to_value(&spec)
            .map_err(|e| EngineError::Protocol(format!("encode spec: {e}")))?;
        let mut rb = self
            .http
            .put(self.url(&format!("/v1/materialized/{name}")))
            .json(&body);
        if let Some(ns) = namespace {
            rb = rb.query(&[("namespace", ns)]);
        }
        let resp = self.send(rb).await?;
        require_namespace_echo(namespace, &resp)?;
        whole(resp)
    }

    async fn drop_materialized(
        &self,
        name: &str,
        namespace: Option<&str>,
    ) -> Result<bool, EngineError> {
        let mut rb = self
            .http
            .delete(self.url(&format!("/v1/materialized/{name}")));
        if let Some(ns) = namespace {
            rb = rb.query(&[("namespace", ns)]);
        }
        let resp = self.send(rb).await?;
        require_namespace_echo(namespace, &resp)?;
        field(&resp, "dropped")
    }

    async fn add_source(&self, def: SourceDef) -> Result<SourceInfo, EngineError> {
        let body = serde_json::to_value(&def)
            .map_err(|e| EngineError::Protocol(format!("encode source: {e}")))?;
        whole(self.post("/v1/sources", body).await?)
    }

    async fn remove_source(&self, name: &str) -> Result<bool, EngineError> {
        let resp = self.delete(&format!("/v1/sources/{name}")).await?;
        field(&resp, "removed")
    }

    async fn test_source(&self, name: &str) -> Result<SourceTestReport, EngineError> {
        whole(self.post_empty(&format!("/v1/sources/{name}/test")).await?)
    }

    async fn reload_config(&self) -> Result<ReloadReport, EngineError> {
        whole(self.post_empty("/v1/config/reload").await?)
    }

    async fn list_semantic_models(&self) -> Result<Vec<SemanticModelInfo>, EngineError> {
        let resp = self.get("/v1/semantic/models").await?;
        field(&resp, "models")
    }

    async fn describe_semantic_model(
        &self,
        name: &str,
    ) -> Result<SemanticModelDescription, EngineError> {
        whole(self.get(&format!("/v1/semantic/models/{name}")).await?)
    }

    async fn semantic_query(&self, q: SemanticQuery) -> Result<QueryHandle, EngineError> {
        let body = serde_json::to_value(&q)
            .map_err(|e| EngineError::Protocol(format!("encode semantic query: {e}")))?;
        let resp = self.post("/v1/query", body).await?;
        json_to_handle(&resp)
    }

    async fn list_functions(&self) -> Result<Vec<FunctionInfo>, EngineError> {
        let resp = self.get("/v1/functions").await?;
        field(&resp, "functions")
    }

    async fn describe_function(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<FunctionDescription, EngineError> {
        whole(
            self.get(&format!("/v1/functions/{namespace}/{name}"))
                .await?,
        )
    }

    async fn health(&self) -> Result<HealthReport, EngineError> {
        whole(self.get("/v1/health").await?)
    }

    async fn shutdown(&self) -> Result<(), EngineError> {
        Err(unsupported("shutdown"))
    }
}
