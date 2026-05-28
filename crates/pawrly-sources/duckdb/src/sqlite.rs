//! SQLite source — registers each declared `tables[].query` (or full table)
//! as a DataFusion `TableProvider` backed by `rusqlite`.
//!
//! Full-table reads with WHERE-equality predicate pushdown into the underlying
//! SQL. Postgres / MySQL via DuckDB extensions are not yet implemented (the
//! integration is symmetrical, just bigger compile cost).

use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;

use arrow_array::builder::{Float64Builder, Int64Builder, StringBuilder};
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use async_trait::async_trait;
use datafusion::catalog::{
    CatalogProvider, MemoryCatalogProvider, MemorySchemaProvider, SchemaProvider, Session,
};
use datafusion::common::DataFusionError;
use datafusion::datasource::memory::MemorySourceConfig;
use datafusion::datasource::{TableProvider, TableType};
use datafusion::execution::context::SessionContext;
use datafusion::logical_expr::{Expr, TableProviderFilterPushDown};
use datafusion::physical_plan::ExecutionPlan;
use parking_lot::Mutex;
use pawrly_core::{ConfigError, SourceDef};
use rusqlite::{Connection, types::Value as SqliteValue};

#[derive(Debug, thiserror::Error)]
pub enum SqliteBuildError {
    #[error("config error: {0}")]
    Config(#[from] ConfigError),

    #[error("`path` is required for kind: sqlite (use ':memory:' for in-process)")]
    MissingPath,

    #[error("rusqlite: {0}")]
    Sqlite(String),

    #[error("datafusion: {0}")]
    DataFusion(String),
}

#[derive(Debug, Clone, Default)]
pub struct SqliteSourceReport {
    pub table_count: u64,
    pub tables: Vec<SqliteTableSummary>,
}

#[derive(Debug, Clone)]
pub struct SqliteTableSummary {
    pub name: String,
    pub description: Option<String>,
}

/// Register a SQLite source. Each entry in `tables[]` becomes a
/// `SqliteTableProvider`. If `tables[]` is empty, every user table found in
/// `sqlite_master` is registered with its native name.
pub async fn register_sqlite_source(
    def: &SourceDef,
    _ctx: &SessionContext,
    catalog: &dyn CatalogProvider,
) -> Result<SqliteSourceReport, SqliteBuildError> {
    let path_str = def
        .config
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or(SqliteBuildError::MissingPath)?;
    let conn = if path_str == ":memory:" {
        Connection::open_in_memory()
    } else {
        Connection::open(PathBuf::from(path_str))
    }
    .map_err(|e| SqliteBuildError::Sqlite(e.to_string()))?;

    let conn = Arc::new(Mutex::new(conn));
    let schema = ensure_schema(catalog, &def.name)?;

    let table_specs: Vec<(String, String, Option<String>)> = if def.tables.is_empty() {
        // Auto-discover user tables.
        let names = list_tables(&conn).map_err(|e| SqliteBuildError::Sqlite(e.to_string()))?;
        names
            .into_iter()
            .map(|n| {
                let q = format!("SELECT * FROM \"{n}\"");
                (n, q, None)
            })
            .collect()
    } else {
        def.tables
            .iter()
            .map(|t| {
                let query = t
                    .config
                    .get("query")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("SELECT * FROM \"{}\"", t.name));
                (t.name.clone(), query, t.description.clone())
            })
            .collect()
    };

    let mut summaries = Vec::with_capacity(table_specs.len());
    for (name, query, description) in table_specs {
        let arrow_schema =
            infer_schema(&conn, &query).map_err(|e| SqliteBuildError::Sqlite(e.to_string()))?;
        let provider = SqliteTableProvider {
            conn: conn.clone(),
            base_query: query,
            schema: arrow_schema,
        };
        schema
            .register_table(name.clone(), Arc::new(provider))
            .map_err(|e| SqliteBuildError::DataFusion(e.to_string()))?;
        summaries.push(SqliteTableSummary { name, description });
    }

    Ok(SqliteSourceReport {
        table_count: summaries.len() as u64,
        tables: summaries,
    })
}

fn ensure_schema(
    catalog: &dyn CatalogProvider,
    name: &str,
) -> Result<Arc<dyn SchemaProvider>, SqliteBuildError> {
    if let Some(s) = catalog.schema(name) {
        return Ok(s);
    }
    let s: Arc<dyn SchemaProvider> = Arc::new(MemorySchemaProvider::new());
    if let Some(memory_catalog) = catalog.as_any().downcast_ref::<MemoryCatalogProvider>() {
        let _ = memory_catalog
            .register_schema(name, s.clone())
            .map_err(|e| SqliteBuildError::DataFusion(e.to_string()))?;
        Ok(s)
    } else {
        Err(SqliteBuildError::DataFusion(
            "catalog does not support schema registration".into(),
        ))
    }
}

fn list_tables(conn: &Mutex<Connection>) -> rusqlite::Result<Vec<String>> {
    let conn = conn.lock();
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn infer_schema(conn: &Mutex<Connection>, query: &str) -> rusqlite::Result<SchemaRef> {
    let conn = conn.lock();
    let stmt = conn.prepare(query)?;
    let column_count = stmt.column_count();
    let mut fields = Vec::with_capacity(column_count);
    for i in 0..column_count {
        let name = stmt.column_name(i)?.to_string();
        // SQLite's affinity is dynamic. Default: everything as Utf8;
        // upgrade based on `PRAGMA table_info` if the column maps to a single
        // base table. (For simplicity we just use Utf8 except for
        // names that look like integer keys.)
        let dtype = if name == "id"
            || name.ends_with("_id")
            || name.ends_with("_count")
            || name.starts_with("is_")
        {
            DataType::Int64
        } else {
            DataType::Utf8
        };
        fields.push(Field::new(name, dtype, true));
    }
    Ok(Arc::new(Schema::new(fields)))
}

#[derive(Debug)]
struct SqliteTableProvider {
    conn: Arc<Mutex<Connection>>,
    base_query: String,
    schema: SchemaRef,
}

impl pawrly_core::DynamicFilterCapable for SqliteTableProvider {
    fn dynamic_filter_columns(&self) -> Vec<String> {
        // SQLite via prepared statements can absorb any column as an IN-list.
        self.schema
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect()
    }
}

#[async_trait]
impl TableProvider for SqliteTableProvider {
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
                if extract_eq_literal(f).is_some() {
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
        limit: Option<usize>,
    ) -> datafusion::common::Result<Arc<dyn ExecutionPlan>> {
        // Build the SQL with pushed-down filters.
        let mut clauses: Vec<String> = Vec::new();
        let mut params: Vec<rusqlite::types::Value> = Vec::new();
        for f in filters {
            if let Some((col, val)) = extract_eq_literal(f) {
                clauses.push(format!("\"{col}\" = ?"));
                params.push(rusqlite::types::Value::Text(val));
            }
        }
        let mut query = self.base_query.clone();
        if !clauses.is_empty() {
            // Wrap in subquery so the WHERE is well-formed regardless of base.
            query = format!("SELECT * FROM ({query}) WHERE {}", clauses.join(" AND "));
        }
        if let Some(n) = limit {
            query.push_str(&format!(" LIMIT {n}"));
        }

        let batch = run_query(&self.conn, &query, &params, self.schema.clone())
            .map_err(|e| DataFusionError::Plan(format!("sqlite: {e}")))?;

        let projected_schema = match projection {
            Some(p) => Arc::new(Schema::new(
                p.iter()
                    .map(|i| self.schema.field(*i).clone())
                    .collect::<Vec<_>>(),
            )),
            None => self.schema.clone(),
        };
        let projected: RecordBatch = if let Some(p) = projection {
            if p.is_empty() {
                use arrow_array::RecordBatchOptions;
                let opts = RecordBatchOptions::new().with_row_count(Some(batch.num_rows()));
                RecordBatch::try_new_with_options(projected_schema.clone(), vec![], &opts)
                    .map_err(|e| DataFusionError::ArrowError(Box::new(e), None))?
            } else {
                let cols: Vec<ArrayRef> = p.iter().map(|i| batch.column(*i).clone()).collect();
                RecordBatch::try_new(projected_schema.clone(), cols)
                    .map_err(|e| DataFusionError::ArrowError(Box::new(e), None))?
            }
        } else {
            batch
        };
        let exec = MemorySourceConfig::try_new_exec(&[vec![projected]], projected_schema, None)
            .map_err(|e| DataFusionError::Plan(e.to_string()))?;
        Ok(exec as Arc<dyn ExecutionPlan>)
    }
}

fn run_query(
    conn: &Mutex<Connection>,
    query: &str,
    params: &[rusqlite::types::Value],
    schema: SchemaRef,
) -> rusqlite::Result<RecordBatch> {
    let conn = conn.lock();
    let mut stmt = conn.prepare(query)?;
    let column_count = stmt.column_count();
    let mut rows = stmt.query(rusqlite::params_from_iter(params.iter()))?;

    // One column-builder per Arrow field.
    let mut builders: Vec<ColumnBuilder> = schema
        .fields()
        .iter()
        .map(|f| ColumnBuilder::new(f.data_type()))
        .collect();

    while let Some(row) = rows.next()? {
        for (i, b) in builders.iter_mut().enumerate().take(column_count) {
            let v: SqliteValue = row.get(i)?;
            b.push(v);
        }
    }

    let arrays: Vec<ArrayRef> = builders.into_iter().map(ColumnBuilder::finish).collect();
    let batch = RecordBatch::try_new(schema, arrays)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    Ok(batch)
}

enum ColumnBuilder {
    Int(Int64Builder),
    Float(Float64Builder),
    Str(StringBuilder),
}

impl ColumnBuilder {
    fn new(dtype: &DataType) -> Self {
        match dtype {
            DataType::Int64 => Self::Int(Int64Builder::new()),
            DataType::Float64 => Self::Float(Float64Builder::new()),
            _ => Self::Str(StringBuilder::new()),
        }
    }

    fn push(&mut self, v: SqliteValue) {
        match (self, v) {
            (Self::Int(b), SqliteValue::Integer(i)) => b.append_value(i),
            (Self::Int(b), SqliteValue::Real(f)) => b.append_value(f as i64),
            (Self::Int(b), SqliteValue::Text(s)) => match s.parse::<i64>() {
                Ok(i) => b.append_value(i),
                Err(_) => b.append_null(),
            },
            (Self::Int(b), _) => b.append_null(),

            (Self::Float(b), SqliteValue::Real(f)) => b.append_value(f),
            (Self::Float(b), SqliteValue::Integer(i)) => b.append_value(i as f64),
            (Self::Float(b), SqliteValue::Text(s)) => match s.parse::<f64>() {
                Ok(f) => b.append_value(f),
                Err(_) => b.append_null(),
            },
            (Self::Float(b), _) => b.append_null(),

            (Self::Str(b), SqliteValue::Text(s)) => b.append_value(&s),
            (Self::Str(b), SqliteValue::Integer(i)) => b.append_value(i.to_string()),
            (Self::Str(b), SqliteValue::Real(f)) => b.append_value(f.to_string()),
            (Self::Str(b), SqliteValue::Blob(_)) => b.append_null(),
            (Self::Str(b), SqliteValue::Null) => b.append_null(),
        }
    }

    fn finish(self) -> ArrayRef {
        match self {
            Self::Int(mut b) => Arc::new(b.finish()),
            Self::Float(mut b) => Arc::new(b.finish()),
            Self::Str(mut b) => Arc::new(b.finish()),
        }
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
            ScalarValue::Boolean(Some(b)) => (if *b { "1" } else { "0" }).to_string(),
            _ => return None,
        };
        return Some((col.name.clone(), value));
    }
    None
}
