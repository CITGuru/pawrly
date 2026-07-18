//! Raw HTTP table provider — exposes a source as a single table named after
//! the source itself, with virtual columns `request_path`, `request_query`,
//! `request_method`, `response_status`, `response_body`.
//!
//! Required filter on `request_path`. Each value supplied via `IN(...)` or
//! equality fans out to one HTTP request.

use std::any::Any;
use std::sync::Arc;

use arrow_array::{
    ArrayRef, RecordBatch,
    builder::{Int32Builder, StringBuilder},
};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use async_trait::async_trait;
use datafusion::catalog::Session;
use datafusion::common::DataFusionError;
use datafusion::datasource::memory::MemorySourceConfig;
use datafusion::datasource::{TableProvider, TableType};
use datafusion::logical_expr::{Expr, TableProviderFilterPushDown};
use datafusion::physical_plan::ExecutionPlan;

use crate::source::{HttpSource, custom_body_object};

#[derive(Debug)]
pub struct RawHttpTableProvider {
    pub source: Arc<HttpSource>,
    pub schema: SchemaRef,
}

impl RawHttpTableProvider {
    pub fn new(source: Arc<HttpSource>) -> Self {
        let schema = Arc::new(Schema::new(vec![
            Field::new("request_method", DataType::Utf8, false),
            Field::new("request_path", DataType::Utf8, false),
            Field::new("request_query", DataType::Utf8, true),
            Field::new("response_status", DataType::Int32, true),
            Field::new("response_body", DataType::Utf8, true),
        ]));
        Self { source, schema }
    }
}

#[async_trait]
impl TableProvider for RawHttpTableProvider {
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
                if mentions_column(f, "request_path") || mentions_column(f, "request_query") {
                    TableProviderFilterPushDown::Exact
                } else {
                    TableProviderFilterPushDown::Unsupported
                }
            })
            .collect())
    }

    async fn scan(
        &self,
        _state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        _limit: Option<usize>,
    ) -> datafusion::common::Result<Arc<dyn ExecutionPlan>> {
        let paths = collect_eq_or_in(filters, "request_path");
        if paths.is_empty() {
            return Err(DataFusionError::Plan(format!(
                "raw HTTP table `{}` requires a filter on `request_path` (PAWRLY_SAFETY_REQUIRED_FILTER)",
                self.source.name
            )));
        }
        let queries = collect_eq_or_in(filters, "request_query");
        let method = collect_eq_or_in(filters, "request_method")
            .into_iter()
            .next()
            .unwrap_or_else(|| "GET".into());
        let method_parsed = method.parse().unwrap_or(reqwest::Method::GET);

        // Build the Cartesian product of (path, query). For each combination
        // we issue one request.
        let request_pairs: Vec<(String, Option<String>)> = if queries.is_empty() {
            paths.iter().map(|p| (p.clone(), None)).collect()
        } else {
            paths
                .iter()
                .flat_map(|p| {
                    let p = p.clone();
                    queries.iter().map(move |q| (p.clone(), Some(q.clone())))
                })
                .collect()
        };

        let mut method_b = StringBuilder::new();
        let mut path_b = StringBuilder::new();
        let mut query_b = StringBuilder::new();
        let mut status_b = Int32Builder::new();
        let mut body_b = StringBuilder::new();

        for (path, query) in request_pairs {
            let mut url = self
                .source
                .base_url
                .join(path.trim_start_matches('/'))
                .map_err(|e| DataFusionError::Plan(format!("bad url: {e}")))?;
            if let Some(q) = &query {
                url.set_query(Some(q));
            }
            crate::guard::check_target(&url, &self.source.base_url, &self.source.allowed_hosts)
                .map_err(DataFusionError::Plan)?;
            let mut req = self.source.client.request(method_parsed.clone(), url);
            for (k, v) in &self.source.headers {
                req = req.header(k, v);
            }
            req = self
                .source
                .apply_auth(req)
                .await
                .map_err(|e| DataFusionError::External(Box::new(std::io::Error::other(e))))?;
            // `custom` auth body fields ride as a JSON body (the raw table has no
            // body of its own to merge with).
            let body_fields = self.source.custom_body_fields();
            if !body_fields.is_empty() {
                req = req.json(&serde_json::Value::Object(custom_body_object(body_fields)));
            }
            let resp = req.send().await.map_err(|e| {
                DataFusionError::External(Box::new(std::io::Error::other(format!("http: {e}"))))
            })?;
            let status = resp.status().as_u16() as i32;
            let body = resp.text().await.unwrap_or_default();
            method_b.append_value(&method);
            path_b.append_value(&path);
            match query {
                Some(q) => query_b.append_value(&q),
                None => query_b.append_null(),
            }
            status_b.append_value(status);
            body_b.append_value(&body);
        }

        let arrays: Vec<ArrayRef> = vec![
            Arc::new(method_b.finish()),
            Arc::new(path_b.finish()),
            Arc::new(query_b.finish()),
            Arc::new(status_b.finish()),
            Arc::new(body_b.finish()),
        ];
        let batch = RecordBatch::try_new(self.schema.clone(), arrays)
            .map_err(|e| DataFusionError::ArrowError(Box::new(e), None))?;

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

fn mentions_column(expr: &Expr, name: &str) -> bool {
    use datafusion::common::tree_node::{TreeNode, TreeNodeRecursion};
    let mut found = false;
    let _ = expr.apply(|e| {
        if let Expr::Column(c) = e
            && c.name == name
        {
            found = true;
            Ok::<_, datafusion::common::DataFusionError>(TreeNodeRecursion::Stop)
        } else {
            Ok(TreeNodeRecursion::Continue)
        }
    });
    found
}

/// Recursively collect every literal value pinned to `column` via either
/// `column = literal` or `column IN (...)`. Tolerates `OR` rewrites of `IN`.
fn collect_eq_or_in(filters: &[Expr], column: &str) -> Vec<String> {
    let mut out = Vec::new();
    for f in filters {
        walk(f, column, &mut out);
    }
    out
}

fn walk(expr: &Expr, column: &str, out: &mut Vec<String>) {
    use datafusion::logical_expr::{BinaryExpr, Operator};
    match expr {
        Expr::BinaryExpr(BinaryExpr { left, op, right }) => match op {
            Operator::Eq => {
                let (col, val) = match (left.as_ref(), right.as_ref()) {
                    (Expr::Column(c), Expr::Literal(s, _)) => (c, s),
                    (Expr::Literal(s, _), Expr::Column(c)) => (c, s),
                    _ => return,
                };
                if col.name == column
                    && let Some(s) = scalar_to_string(val)
                {
                    out.push(s);
                }
            }
            Operator::Or | Operator::And => {
                walk(left, column, out);
                walk(right, column, out);
            }
            _ => {}
        },
        Expr::InList(in_list) if !in_list.negated => {
            if let Expr::Column(c) = in_list.expr.as_ref()
                && c.name == column
            {
                for item in &in_list.list {
                    if let Expr::Literal(s, _) = item
                        && let Some(s) = scalar_to_string(s)
                    {
                        out.push(s);
                    }
                }
            }
        }
        _ => {}
    }
}

fn scalar_to_string(s: &datafusion::scalar::ScalarValue) -> Option<String> {
    use datafusion::scalar::ScalarValue;
    match s {
        ScalarValue::Utf8(Some(v)) | ScalarValue::LargeUtf8(Some(v)) => Some(v.clone()),
        ScalarValue::Int64(Some(n)) => Some(n.to_string()),
        ScalarValue::Int32(Some(n)) => Some(n.to_string()),
        _ => None,
    }
}
