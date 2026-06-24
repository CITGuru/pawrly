//! `TableProvider` that backs a table with `tools/call`: pushed-down `WHERE`
//! filters become tool arguments, the result's rows are parsed into Arrow.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use arrow_array::{
    ArrayRef, BooleanArray, Float64Array, Int64Array, RecordBatch, RecordBatchOptions, StringArray,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use async_trait::async_trait;
use datafusion::catalog::Session;
use datafusion::common::DataFusionError;
use datafusion::datasource::memory::MemorySourceConfig;
use datafusion::datasource::{TableProvider, TableType};
use datafusion::logical_expr::{BinaryExpr, Expr, Operator, TableProviderFilterPushDown};
use datafusion::physical_plan::ExecutionPlan;
use serde_json::Value;

use crate::session::McpClientSession;
use crate::synth::McpTableSpec;

const DEFAULT_MAX_PAGES: u32 = 50;

pub struct McpToolTableProvider {
    session: Arc<McpClientSession>,
    spec: Arc<McpTableSpec>,
    schema: SchemaRef,
    max_pages: u32,
}

impl std::fmt::Debug for McpToolTableProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToolTableProvider")
            .field("tool", &self.spec.tool)
            .field("name", &self.spec.name)
            .finish()
    }
}

impl McpToolTableProvider {
    pub fn new(
        session: Arc<McpClientSession>,
        spec: Arc<McpTableSpec>,
        max_pages: Option<u32>,
    ) -> Self {
        let schema = build_schema(&spec);
        Self {
            session,
            spec,
            schema,
            max_pages: max_pages.unwrap_or(DEFAULT_MAX_PAGES),
        }
    }
}

/// Whether an arg is shadowed by an output column of the same name; the output
/// column wins (the arg stays bindable for the tool call and filter pushdown).
fn arg_is_shadowed(spec: &McpTableSpec, arg: &str) -> bool {
    spec.columns.iter().any(|c| c.name == arg)
}

/// Schema = arg columns (echoed) followed by output columns; an arg shadowed by
/// an output column is omitted to avoid a duplicate field.
fn build_schema(spec: &McpTableSpec) -> SchemaRef {
    let mut fields: Vec<Field> = Vec::with_capacity(spec.args.len() + spec.columns.len());
    for arg in &spec.args {
        if !arg_is_shadowed(spec, &arg.name) {
            fields.push(Field::new(&arg.name, arrow_type(&arg.r#type), true));
        }
    }
    for col in &spec.columns {
        fields.push(Field::new(&col.name, arrow_type(&col.r#type), true));
    }
    Arc::new(Schema::new(fields))
}

pub(crate) fn arrow_type(t: &str) -> DataType {
    match t.to_ascii_lowercase().as_str() {
        "bool" | "boolean" => DataType::Boolean,
        "int" | "int32" | "bigint" | "int64" | "long" => DataType::Int64,
        "float" | "float32" | "double" | "float64" => DataType::Float64,
        _ => DataType::Utf8,
    }
}

#[async_trait]
impl TableProvider for McpToolTableProvider {
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
            .map(|f| match extract_eq(f) {
                // Bound to a tool argument; DataFusion re-checks on the echoed column.
                Some((col, _)) if self.spec.args.iter().any(|a| a.name == col) => {
                    TableProviderFilterPushDown::Inexact
                }
                _ => TableProviderFilterPushDown::Unsupported,
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
        let bound = bound_args(&self.spec, filters);
        let rows = self
            .fetch(&bound, limit)
            .await
            .map_err(|e| DataFusionError::Execution(e.to_string()))?;

        let batch = build_batch(&self.schema, &self.spec, &bound, &rows)?;
        let (schema, batch) = project(&self.schema, batch, projection)?;
        let exec = MemorySourceConfig::try_new_exec(&[vec![batch]], schema, None)
            .map_err(|e| DataFusionError::Plan(e.to_string()))?;
        Ok(exec)
    }
}

impl McpToolTableProvider {
    /// Run the `tools/call` pipeline for an already-bound argument set and
    /// return the extracted rows (static `tool_args` + bound args by wire name +
    /// `limit_binding`, cursor pagination, `rows_path` extraction). The MCP
    /// function executor reuses this to build a `returns`-shaped batch instead of
    /// the auto-echo table batch.
    pub(crate) async fn fetch(
        &self,
        bound: &BTreeMap<String, Value>,
        limit: Option<usize>,
    ) -> Result<Vec<Value>, crate::error::McpError> {
        let mut base = serde_json::Map::new();
        for (k, v) in &self.spec.tool_args {
            base.insert(k.clone(), v.clone());
        }
        for arg in &self.spec.args {
            if let Some(value) = bound.get(&arg.name) {
                base.insert(arg.wire_name().to_string(), value.clone());
            }
        }
        if let (Some(binding), Some(limit)) = (&self.spec.limit_binding, limit) {
            let capped = binding.max.map_or(limit, |m| limit.min(m));
            base.insert(binding.tool_arg.clone(), Value::from(capped as u64));
        }

        let mut all_rows = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..self.max_pages {
            let mut args = base.clone();
            if let (Some(pagination), Some(c)) = (&self.spec.pagination, &cursor) {
                args.insert(pagination.cursor_arg.clone(), Value::String(c.clone()));
            }
            let result = self
                .session
                .call_tool(&self.spec.tool, Value::Object(args))
                .await?;
            let structured = structured_result(&result);
            all_rows.extend(rows_from(&structured, &self.spec.rows_path));

            if limit.is_some_and(|l| all_rows.len() >= l) {
                break;
            }
            match &self.spec.pagination {
                Some(pagination) => match walk(&result, &pagination.response_cursor_path)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    Some(next) => cursor = Some(next.to_string()),
                    None => break,
                },
                None => break,
            }
        }
        if let Some(l) = limit {
            all_rows.truncate(l);
        }
        Ok(all_rows)
    }
}

/// Extract `col = literal` filters into a name → JSON-value map (args only).
fn bound_args(spec: &McpTableSpec, filters: &[Expr]) -> BTreeMap<String, Value> {
    let mut bound = BTreeMap::new();
    for filter in filters {
        if let Some((col, value)) = extract_eq(filter)
            && spec.args.iter().any(|a| a.name == col)
        {
            bound.insert(col, value);
        }
    }
    bound
}

fn extract_eq(expr: &Expr) -> Option<(String, Value)> {
    let Expr::BinaryExpr(BinaryExpr { left, op, right }) = expr else {
        return None;
    };
    if *op != Operator::Eq {
        return None;
    }
    match (left.as_ref(), right.as_ref()) {
        (Expr::Column(c), Expr::Literal(s, _)) | (Expr::Literal(s, _), Expr::Column(c)) => {
            Some((c.name.clone(), scalar_to_json(s)))
        }
        _ => None,
    }
}

fn scalar_to_json(scalar: &datafusion::scalar::ScalarValue) -> Value {
    use datafusion::scalar::ScalarValue;
    match scalar {
        ScalarValue::Utf8(Some(s)) | ScalarValue::LargeUtf8(Some(s)) => Value::String(s.clone()),
        ScalarValue::Int32(Some(n)) => Value::from(*n),
        ScalarValue::Int64(Some(n)) => Value::from(*n),
        ScalarValue::UInt64(Some(n)) => Value::from(*n),
        ScalarValue::Boolean(Some(b)) => Value::Bool(*b),
        ScalarValue::Float64(Some(f)) => Value::from(*f),
        _ => Value::Null,
    }
}

/// The structured payload of a `tools/call` result: `structuredContent` when
/// present, otherwise the text content unwrapped — a lone text block that parses
/// as JSON is used as the payload (the common "JSON-in-a-text-block" shape).
fn structured_result(result: &Value) -> Value {
    if let Some(structured) = result.get("structuredContent") {
        return structured.clone();
    }
    let Some(items) = result.get("content").and_then(Value::as_array) else {
        return Value::Null;
    };
    let texts: Vec<&str> = items
        .iter()
        .filter(|i| i.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|i| i.get("text").and_then(Value::as_str))
        .collect();
    match texts.as_slice() {
        [only] => {
            serde_json::from_str::<Value>(only).unwrap_or_else(|_| Value::String((*only).into()))
        }
        [] => result.get("content").cloned().unwrap_or(Value::Null),
        many => Value::String(many.join("\n")),
    }
}

/// Walk a key path into a JSON value, returning the rows array (or the value as
/// a single row).
fn rows_from(structured: &Value, path: &[String]) -> Vec<Value> {
    let Some(target) = walk(structured, path) else {
        return Vec::new();
    };
    match target {
        Value::Array(rows) => rows.clone(),
        Value::Object(map) => {
            let arrays: Vec<&Value> = map.values().filter(|v| v.is_array()).collect();
            match arrays.as_slice() {
                [only] => only.as_array().cloned().unwrap_or_default(),
                _ => vec![target.clone()],
            }
        }
        Value::Null => Vec::new(),
        other => vec![other.clone()],
    }
}

pub(crate) fn walk<'a>(value: &'a Value, path: &[String]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = current.get(key)?;
    }
    Some(current)
}

fn build_batch(
    schema: &SchemaRef,
    spec: &McpTableSpec,
    bound: &BTreeMap<String, Value>,
    rows: &[Value],
) -> datafusion::common::Result<RecordBatch> {
    let n = rows.len();
    let mut arrays: Vec<ArrayRef> = Vec::with_capacity(schema.fields().len());

    for arg in &spec.args {
        if arg_is_shadowed(spec, &arg.name) {
            continue;
        }
        let value = bound.get(&arg.name).cloned().unwrap_or(Value::Null);
        let column: Vec<Value> = std::iter::repeat_n(value, n).collect();
        arrays.push(column_array(
            schema.field_with_name(&arg.name)?.data_type(),
            &column,
        ));
    }
    for col in &spec.columns {
        let column: Vec<Value> = rows
            .iter()
            .map(|row| walk(row, &col.path).cloned().unwrap_or(Value::Null))
            .collect();
        arrays.push(column_array(
            schema.field_with_name(&col.name)?.data_type(),
            &column,
        ));
    }

    RecordBatch::try_new(schema.clone(), arrays)
        .map_err(|e| DataFusionError::ArrowError(Box::new(e), None))
}

pub(crate) fn column_array(data_type: &DataType, values: &[Value]) -> ArrayRef {
    match data_type {
        DataType::Int64 => Arc::new(Int64Array::from_iter(values.iter().map(|v| match v {
            Value::Number(n) => n.as_i64(),
            Value::String(s) => s.parse().ok(),
            _ => None,
        }))),
        DataType::Float64 => Arc::new(Float64Array::from_iter(values.iter().map(|v| match v {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.parse().ok(),
            _ => None,
        }))),
        DataType::Boolean => Arc::new(BooleanArray::from_iter(values.iter().map(Value::as_bool))),
        _ => Arc::new(StringArray::from_iter(values.iter().map(|v| match v {
            Value::Null => None,
            Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        }))),
    }
}

fn project(
    schema: &SchemaRef,
    batch: RecordBatch,
    projection: Option<&Vec<usize>>,
) -> datafusion::common::Result<(SchemaRef, RecordBatch)> {
    let Some(indices) = projection else {
        return Ok((schema.clone(), batch));
    };
    let fields: Vec<Field> = indices.iter().map(|i| schema.field(*i).clone()).collect();
    let projected_schema = Arc::new(Schema::new(fields));
    let columns: Vec<ArrayRef> = indices.iter().map(|i| batch.column(*i).clone()).collect();
    // A zero-column projection (e.g. COUNT(*)) has no array to infer the row
    // count from, so carry it explicitly.
    let options = RecordBatchOptions::new().with_row_count(Some(batch.num_rows()));
    let projected = RecordBatch::try_new_with_options(projected_schema.clone(), columns, &options)
        .map_err(|e| DataFusionError::ArrowError(Box::new(e), None))?;
    Ok((projected_schema, projected))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn structured_result_prefers_structured_then_unwraps_text() {
        assert_eq!(
            structured_result(&json!({ "structuredContent": { "teams": [] } })),
            json!({ "teams": [] })
        );
        // A lone text block that is JSON is parsed (Linear's shape).
        assert_eq!(
            structured_result(&json!({ "content": [{ "type": "text", "text": "{\"a\":1}" }] })),
            json!({ "a": 1 })
        );
        // Non-JSON text stays a string.
        assert_eq!(
            structured_result(&json!({ "content": [{ "type": "text", "text": "hi" }] })),
            json!("hi")
        );
    }

    #[test]
    fn rows_from_classifies_arrays_objects_and_paths() {
        assert_eq!(rows_from(&json!([1, 2]), &[]).len(), 2);
        assert_eq!(rows_from(&json!({ "items": [1, 2, 3] }), &[]).len(), 3);
        assert_eq!(rows_from(&json!({ "id": 1 }), &[]).len(), 1);
        assert_eq!(
            rows_from(&json!({ "data": { "rows": [1] } }), &["data".into()]).len(),
            1
        );
    }

    #[test]
    fn output_column_shadows_same_named_arg() {
        use crate::synth::{Arg, Column};
        let arg = |name: &str| Arg {
            name: name.into(),
            r#type: "varchar".into(),
            required: false,
            tool_arg: None,
        };
        let spec = McpTableSpec {
            name: "t".into(),
            tool: "t".into(),
            description: None,
            args: vec![arg("assignee"), arg("team")],
            columns: vec![Column {
                name: "team".into(),
                r#type: "varchar".into(),
                path: vec!["team".into()],
            }],
            tool_args: Default::default(),
            limit_binding: None,
            pagination: None,
            rows_path: Vec::new(),
        };
        let names: Vec<String> = build_schema(&spec)
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect();
        // `team` appears once (the output column), `assignee` once (the arg).
        assert_eq!(names, ["assignee", "team"]);
    }
}
