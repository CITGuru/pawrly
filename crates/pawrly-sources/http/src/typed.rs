//! Typed HTTP table provider: one declared endpoint with declared columns.
//!
//! Simplifications:
//! - Filter pushdown is done by lifting `WHERE col = literal` filters that
//!   match a declared parameter and substituting them into the URL path /
//!   query string.
//! - No pagination; we fetch the first page only.
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

use crate::source::{AuthSpec, HttpSource, HttpTableSpec, ResponseColumn, schema_for};

#[derive(Debug)]
pub struct HttpTableProvider {
    pub source: Arc<HttpSource>,
    pub spec: Arc<HttpTableSpec>,
    pub schema: SchemaRef,
}

impl pawrly_core::DynamicFilterCapable for HttpTableProvider {
    fn dynamic_filter_columns(&self) -> Vec<String> {
        // Declared params can absorb runtime `IN(...)` filters on equality.
        self.spec.params.iter().map(|p| p.name.clone()).collect()
    }
}

impl HttpTableProvider {
    pub fn new(source: Arc<HttpSource>, spec: Arc<HttpTableSpec>) -> Self {
        let schema = schema_for(&spec);
        Self {
            source,
            spec,
            schema,
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

        let url = build_url(&self.source.base_url, &self.spec.endpoint, &params)?;

        let mut req = self.source.client.request(
            self.spec.method.parse().unwrap_or(reqwest::Method::GET),
            url,
        );
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

        let resp = req.send().await.map_err(|e| {
            DataFusionError::External(Box::new(std::io::Error::other(format!(
                "http request failed: {e}"
            ))))
        })?;
        let status = resp.status();
        let body: Value = resp.json().await.map_err(|e| {
            DataFusionError::External(Box::new(std::io::Error::other(format!(
                "json parse failed (status {status}): {e}"
            ))))
        })?;

        let rows = extract_rows(&body, &self.spec.response.path)?;

        let batch = build_batch(&self.schema, &self.spec.response.schema, &rows, &params)?;
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
