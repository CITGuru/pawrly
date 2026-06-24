//! MCP table-valued function executor.
//!
//! Reuses the `tools/call` pipeline (see [`McpToolTableProvider::fetch`]) for
//! the actual request + pagination + row extraction, but — unlike an MCP-source
//! table — builds a batch whose columns come *only* from the function's declared
//! `returns`. A table auto-echoes every bound arg as a column; a function echoes
//! an arg only when a `returns` column names it with `source: arg`, mirroring the
//! HTTP function executor.

use std::collections::BTreeMap;
use std::sync::Arc;

use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{Field, Schema, SchemaRef};
use datafusion::common::DataFusionError;
use pawrly_core::{FunctionArg, FunctionColumn, FunctionDef};
use serde_json::{Map, Value};

use crate::error::McpBuildError;
use crate::provider::{McpToolTableProvider, arrow_type, column_array, walk};
use crate::session::McpClientSession;
use crate::synth::McpTableSpec;

/// Executes one MCP table-valued function.
pub struct McpFunctionExecutor {
    /// Carries the shared session + assembled spec; only its `fetch` (rows) is
    /// used — the function builds its own `returns`-shaped batch.
    provider: McpToolTableProvider,
    args: Vec<FunctionArg>,
    returns: Vec<FunctionColumn>,
    schema: SchemaRef,
}

impl McpFunctionExecutor {
    /// Build an executor over a (shared or freshly connected) session and a
    /// function definition.
    pub fn new(
        session: Arc<McpClientSession>,
        def: &FunctionDef,
        max_pages: Option<u32>,
    ) -> Result<Self, McpBuildError> {
        let spec = function_spec(def)?;
        let schema = function_schema(&def.returns);
        let provider = McpToolTableProvider::new(session, Arc::new(spec), max_pages);
        Ok(Self {
            provider,
            args: def.args.clone(),
            returns: def.returns.clone(),
            schema,
        })
    }

    /// The output schema (from `returns`), known at plan time.
    pub fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    /// Run the function for one fully-bound argument set.
    pub async fn invoke(
        &self,
        params: &BTreeMap<String, String>,
        limit: Option<usize>,
    ) -> datafusion::common::Result<RecordBatch> {
        let bound = bind_args(&self.args, params);
        let rows = self
            .provider
            .fetch(&bound, limit)
            .await
            .map_err(|e| DataFusionError::Execution(e.to_string()))?;
        build_function_batch(&self.schema, &self.returns, &bound, &rows)
    }
}

/// Type the string call args into JSON values by their declared arg type, keyed
/// by the (spec) arg name; `fetch` then maps each to its `tool_arg` wire name.
fn bind_args(args: &[FunctionArg], params: &BTreeMap<String, String>) -> BTreeMap<String, Value> {
    let mut bound = BTreeMap::new();
    for a in args {
        if let Some(v) = params.get(&a.name) {
            bound.insert(a.name.clone(), typed_json(&a.r#type, v));
        }
    }
    bound
}

fn typed_json(type_str: &str, value: &str) -> Value {
    match type_str.trim().to_ascii_lowercase().as_str() {
        "int" | "integer" | "int32" | "int64" | "bigint" | "long" | "smallint" => value
            .parse::<i64>()
            .map_or_else(|_| Value::String(value.to_string()), Value::from),
        "double" | "float" | "float32" | "float64" | "real" | "decimal" | "numeric" => value
            .parse::<f64>()
            .map_or_else(|_| Value::String(value.to_string()), Value::from),
        "bool" | "boolean" => value
            .parse::<bool>()
            .map_or_else(|_| Value::String(value.to_string()), Value::Bool),
        _ => Value::String(value.to_string()),
    }
}

/// Assemble an [`McpTableSpec`] from a function definition. The body carries the
/// tool mapping (`tool`, `tool_args`, `rows_path`, `pagination`,
/// `limit_binding`); `args` become the spec args (with `tool_arg` wire names);
/// `columns` is left empty (the executor builds a `returns`-shaped batch, so no
/// implicit arg-echo columns appear).
fn function_spec(def: &FunctionDef) -> Result<McpTableSpec, McpBuildError> {
    let mut map = match def.body.clone() {
        Value::Object(m) => m,
        Value::Null => Map::new(),
        _ => return Err(invalid(def, "mcp function `body` must be a mapping")),
    };
    map.insert("name".to_string(), Value::String(def.name.clone()));
    if let Some(desc) = &def.description {
        map.entry("description")
            .or_insert_with(|| Value::String(desc.clone()));
    }

    let args: Vec<Value> = def
        .args
        .iter()
        .map(|a| {
            let mut o = Map::new();
            o.insert("name".to_string(), Value::String(a.name.clone()));
            o.insert("type".to_string(), Value::String(a.r#type.clone()));
            if a.required {
                o.insert("required".to_string(), Value::Bool(true));
            }
            if let Some(t) = &a.tool_arg {
                o.insert("tool_arg".to_string(), Value::String(t.clone()));
            }
            Value::Object(o)
        })
        .collect();
    map.insert("args".to_string(), Value::Array(args));
    // Output columns are not driven by the spec; force an empty list so the
    // shared schema/batch builders (if ever reached) echo nothing.
    map.insert("columns".to_string(), Value::Array(Vec::new()));

    serde_json::from_value(Value::Object(map))
        .map_err(|e| invalid(def, &format!("invalid mcp function body: {e}")))
}

fn function_schema(returns: &[FunctionColumn]) -> SchemaRef {
    let fields: Vec<Field> = returns
        .iter()
        .map(|c| Field::new(&c.name, arrow_type(&c.r#type), true))
        .collect();
    Arc::new(Schema::new(fields))
}

/// Build a batch whose columns are exactly the function's `returns`:
/// `source: arg` injects the bound call argument as a constant column; any other
/// `source` (a `$.a.b` JSONPath, or the column name by default) is pulled from
/// each row.
fn build_function_batch(
    schema: &SchemaRef,
    returns: &[FunctionColumn],
    bound: &BTreeMap<String, Value>,
    rows: &[Value],
) -> datafusion::common::Result<RecordBatch> {
    let n = rows.len();
    let mut arrays: Vec<ArrayRef> = Vec::with_capacity(returns.len());
    for c in returns {
        let values: Vec<Value> = match c.source.as_deref() {
            Some("arg") => {
                let v = bound.get(&c.name).cloned().unwrap_or(Value::Null);
                std::iter::repeat_n(v, n).collect()
            }
            src => {
                let segments = path_segments(src, &c.name);
                rows.iter()
                    .map(|row| walk(row, &segments).cloned().unwrap_or(Value::Null))
                    .collect()
            }
        };
        let field = schema
            .field_with_name(&c.name)
            .map_err(|e| DataFusionError::ArrowError(Box::new(e), None))?;
        arrays.push(column_array(field.data_type(), &values));
    }
    RecordBatch::try_new(schema.clone(), arrays)
        .map_err(|e| DataFusionError::ArrowError(Box::new(e), None))
}

/// Convert a `returns` column `source` into MCP row path segments. `None`
/// defaults to the column name; a `$.a.b` / `a.b` JSONPath splits on `.`; `$`
/// (or empty) means the whole row.
fn path_segments(src: Option<&str>, default_name: &str) -> Vec<String> {
    match src {
        None => vec![default_name.to_string()],
        Some(s) => {
            let trimmed = s.trim_start_matches('$').trim_start_matches('.');
            if trimmed.is_empty() {
                Vec::new()
            } else {
                trimmed.split('.').map(str::to_string).collect()
            }
        }
    }
}

fn invalid(def: &FunctionDef, msg: &str) -> McpBuildError {
    McpBuildError::Config(format!("function `{}.{}`: {msg}", def.namespace, def.name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pawrly_core::FunctionKind;
    use serde_json::json;

    fn arg(name: &str, ty: &str, tool_arg: Option<&str>) -> FunctionArg {
        FunctionArg {
            name: name.into(),
            r#type: ty.into(),
            required: false,
            default: None,
            description: None,
            tool_arg: tool_arg.map(str::to_string),
        }
    }

    fn col(name: &str, ty: &str, source: Option<&str>) -> FunctionColumn {
        FunctionColumn {
            name: name.into(),
            r#type: ty.into(),
            source: source.map(str::to_string),
            description: None,
        }
    }

    fn def() -> FunctionDef {
        FunctionDef {
            namespace: "linear".into(),
            name: "search".into(),
            kind: FunctionKind::Mcp,
            description: None,
            wiki: None,
            examples: vec![],
            args: vec![
                arg("q", "varchar", Some("query")),
                arg("limit", "int", None),
            ],
            returns: vec![
                col("key", "varchar", None),
                col("title", "varchar", Some("$.fields.title")),
                col("q", "varchar", Some("arg")),
            ],
            connection: Value::Null,
            body: json!({
                "tool": "search",
                "tool_args": { "state": "open" },
                "rows_path": ["issues"],
                "pagination": { "cursor_arg": "cursor", "response_cursor_path": ["nextCursor"] }
            }),
            source: Some("linear".into()),
            builtin: false,
            cache: Default::default(),
            safety: None,
        }
    }

    #[test]
    fn function_spec_maps_tool_args_and_wire_names() {
        let spec = function_spec(&def()).expect("spec");
        assert_eq!(spec.tool, "search");
        assert_eq!(
            spec.tool_args.get("state").and_then(Value::as_str),
            Some("open")
        );
        assert_eq!(spec.rows_path, vec!["issues".to_string()]);
        assert_eq!(spec.args.len(), 2);
        assert_eq!(spec.args[0].wire_name(), "query"); // tool_arg rename
        assert_eq!(spec.args[1].wire_name(), "limit"); // defaults to the name
        assert!(spec.columns.is_empty()); // no implicit echo
    }

    #[test]
    fn typed_json_coerces_by_declared_type() {
        assert_eq!(typed_json("int", "50"), json!(50));
        assert_eq!(typed_json("bool", "true"), json!(true));
        assert_eq!(typed_json("varchar", "open"), json!("open"));
    }

    #[test]
    fn build_function_batch_echoes_arg_and_extracts_path() {
        let schema = function_schema(&def().returns);
        let mut bound = BTreeMap::new();
        bound.insert("q".to_string(), json!("is:open"));
        let rows = vec![
            json!({ "key": "ENG-1", "fields": { "title": "first" } }),
            json!({ "key": "ENG-2", "fields": { "title": "second" } }),
        ];
        let batch = build_function_batch(&schema, &def().returns, &bound, &rows).expect("batch");
        assert_eq!(batch.num_rows(), 2);
        assert_eq!(batch.num_columns(), 3);

        let keys = batch
            .column_by_name("key")
            .and_then(|a| a.as_any().downcast_ref::<arrow_array::StringArray>())
            .expect("key col");
        assert_eq!(keys.value(0), "ENG-1");

        let titles = batch
            .column_by_name("title")
            .and_then(|a| a.as_any().downcast_ref::<arrow_array::StringArray>())
            .expect("title col");
        assert_eq!(titles.value(1), "second");

        // `source: arg` echoes the bound q for every row.
        let qs = batch
            .column_by_name("q")
            .and_then(|a| a.as_any().downcast_ref::<arrow_array::StringArray>())
            .expect("q col");
        assert_eq!(qs.value(0), "is:open");
        assert_eq!(qs.value(1), "is:open");
    }
}
