//! Tool definitions and dispatch. Each tool maps to one or two
//! `EngineService` calls.

use std::sync::Arc;

use arrow_array::{Array, RecordBatch};
use arrow_cast::display::{ArrayFormatter, FormatOptions};
use pawrly_core::{EngineService, EngineServiceExt};
use serde_json::{Value, json};

/// Stable list of tool descriptors returned by `tools/list`.
pub fn list_tools() -> Vec<Value> {
    vec![
        json!({
            "name": "list_tables",
            "description": "List every table across configured sources.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source": { "type": "string", "description": "Limit to one source" }
                }
            }
        }),
        json!({
            "name": "query",
            "description": "Run a SQL query and return rows as { columns, rows, row_count }.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "sql": { "type": "string" },
                    "max_rows": { "type": "integer", "default": 1000 }
                },
                "required": ["sql"]
            }
        }),
        json!({
            "name": "list_sources",
            "description": "List all configured data sources, their kinds, status, and table counts.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "describe_table",
            "description": "Column schema, descriptions, pushdown affordances, and example \
                            queries for one table. `table` is fully qualified `<schema>.<table>`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "table": {
                        "type": "string",
                        "description": "Fully qualified `<schema>.<table>`"
                    }
                },
                "required": ["table"]
            }
        }),
        json!({
            "name": "get_schema",
            "description": "Compact catalog overview for grounding an LLM: every schema, its \
                            tables, and a one-line column list per table. Optionally limit to \
                            named sources.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "sources": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Limit to these source names (default: all)"
                    },
                    "compact": { "type": "boolean", "default": true }
                }
            }
        }),
        json!({
            "name": "refresh_table",
            "description": "Force a refresh of a cached table now. `table` is fully qualified \
                            `<schema>.<table>`. Only valid for tables with caching enabled.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "table": {
                        "type": "string",
                        "description": "Fully qualified `<schema>.<table>`"
                    }
                },
                "required": ["table"]
            }
        }),
        json!({
            "name": "list_semantic_models",
            "description": "List the semantic-layer models (business vocabulary) with their \
                            dimension and measure counts.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "describe_semantic_model",
            "description": "Full spec for one semantic model: dimensions, measures, \
                            relationships, named segments (reusable filter sets you can \
                            pass in `segments`), and any required filters to satisfy up-front.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Model name" }
                },
                "required": ["name"]
            }
        }),
        json!({
            "name": "semantic_query",
            "description": "Run a structured query against the semantic layer. Members are \
                            `model.dimension` / `model.measure` (dimensions may carry a grain, \
                            e.g. `orders.order_date.month`). Returns { columns, rows, row_count }.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "measures": { "type": "array", "items": { "type": "string" } },
                    "dimensions": { "type": "array", "items": { "type": "string" } },
                    "filters": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "member": { "type": "string" },
                                "op": { "type": "string" },
                                "values": { "type": "array", "items": { "type": "string" } }
                            },
                            "required": ["member", "op"]
                        }
                    },
                    "order_by": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "member": { "type": "string" },
                                "direction": { "type": "string", "enum": ["asc", "desc"] }
                            },
                            "required": ["member"]
                        }
                    },
                    "limit": { "type": "integer" },
                    "time_zone": { "type": "string" },
                    "params": {
                        "type": "object",
                        "additionalProperties": { "type": "string" },
                        "description": "Values bound to ${param:NAME} placeholders (e.g. RLS)."
                    },
                    "max_rows": { "type": "integer", "default": 1000 }
                }
            }
        }),
        json!({
            "name": "materialize",
            "description": "Persist data as a named, self-backed table queryable as \
                            `<namespace>.materialized.<name>`. Provide exactly one origin: \
                            `sql` (a query), `file` (a local CSV/Parquet/JSON path), or `url` \
                            (an http(s) file). Returns { name, file_path, row_count, size_bytes }. \
                            Create-or-replace by name; the table is pinned (never auto-evicted).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Table name (a plain identifier)" },
                    "sql": { "type": "string", "description": "Origin: a SQL query" },
                    "file": { "type": "string", "description": "Origin: a local file path" },
                    "url": { "type": "string", "description": "Origin: an http(s) file URL" },
                    "format": {
                        "type": "string",
                        "enum": ["parquet", "csv", "json"],
                        "description": "Format for file/url; inferred from the extension if omitted"
                    },
                    "params": {
                        "type": "object",
                        "additionalProperties": { "type": "string" },
                        "description": "Values bound to ${param:NAME} placeholders in `sql`"
                    }
                },
                "required": ["name"]
            }
        }),
        json!({
            "name": "drop_materialized",
            "description": "Drop a materialized table by name. Returns { dropped } \
                            (false if no such table existed).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" }
                },
                "required": ["name"]
            }
        }),
    ]
}

/// Dispatch a single `tools/call`.
pub async fn call_tool(
    engine: &Arc<dyn EngineService>,
    name: &str,
    args: &Value,
) -> Result<Value, ToolError> {
    match name {
        "list_tables" => {
            let filter =
                args.get("source")
                    .and_then(|v| v.as_str())
                    .map(|s| pawrly_core::TableFilter {
                        source: Some(s.to_string()),
                        name_glob: None,
                    });
            let tables = engine
                .list_tables(filter)
                .await
                .map_err(|e| ToolError::Engine(e.to_string()))?;
            let rows: Vec<Value> = tables
                .into_iter()
                .map(|t| {
                    json!({
                        "schema": t.name.schema,
                        "name": t.name.table,
                        "kind": t.kind.to_string(),
                        "description": t.description,
                        "cached": t.cached,
                        "required_filters": t.required_filters,
                    })
                })
                .collect();
            Ok(json!({ "tables": rows }))
        }
        "query" => {
            let sql = args
                .get("sql")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ToolError::BadArgs("`sql` is required".into()))?;
            let max = args
                .get("max_rows")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(1000);
            let batches = engine
                .query_collect(sql)
                .await
                .map_err(|e| ToolError::Engine(e.to_string()))?;

            let (columns, rows, total, truncated) = format_batches(&batches, max as usize);
            Ok(json!({
                "columns": columns,
                "rows": rows,
                "row_count": total,
                "truncated": truncated,
            }))
        }
        "list_sources" => {
            let sources = engine
                .list_sources()
                .await
                .map_err(|e| ToolError::Engine(e.to_string()))?;
            let rows =
                serde_json::to_value(&sources).map_err(|e| ToolError::Engine(e.to_string()))?;
            Ok(json!({ "sources": rows }))
        }
        "describe_table" => {
            let table = table_name_arg(args)?;
            let desc = engine
                .describe_table(&table)
                .await
                .map_err(|e| ToolError::Engine(e.to_string()))?;
            serde_json::to_value(&desc).map_err(|e| ToolError::Engine(e.to_string()))
        }
        "get_schema" => {
            let sources = match args.get("sources") {
                Some(v) if !v.is_null() => Some(
                    serde_json::from_value::<Vec<String>>(v.clone())
                        .map_err(|e| ToolError::BadArgs(format!("invalid `sources`: {e}")))?,
                ),
                _ => None,
            };
            let compact = args
                .get("compact")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true);
            let snapshot = engine
                .schema_snapshot(sources, compact)
                .await
                .map_err(|e| ToolError::Engine(e.to_string()))?;
            serde_json::to_value(&snapshot).map_err(|e| ToolError::Engine(e.to_string()))
        }
        "refresh_table" => {
            let table = table_name_arg(args)?;
            let outcome = engine
                .refresh_table(&table)
                .await
                .map_err(|e| ToolError::Engine(e.to_string()))?;
            serde_json::to_value(&outcome).map_err(|e| ToolError::Engine(e.to_string()))
        }
        "list_semantic_models" => {
            let models = engine
                .list_semantic_models()
                .await
                .map_err(|e| ToolError::Engine(e.to_string()))?;
            let rows: Vec<Value> = models
                .into_iter()
                .map(|m| {
                    json!({
                        "name": m.name,
                        "description": m.description,
                        "source": m.source,
                        "dimension_count": m.dimension_count,
                        "measure_count": m.measure_count,
                    })
                })
                .collect();
            Ok(json!({ "models": rows }))
        }
        "describe_semantic_model" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ToolError::BadArgs("`name` is required".into()))?;
            let m = engine
                .describe_semantic_model(name)
                .await
                .map_err(|e| ToolError::Engine(e.to_string()))?;
            serde_json::to_value(&m).map_err(|e| ToolError::Engine(e.to_string()))
        }
        "semantic_query" => {
            let max = args
                .get("max_rows")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(1000);
            // The tool args are a superset of `SemanticQuery`'s fields (plus
            // `max_rows`), which it deserializes from directly.
            let q: pawrly_core::semantic::SemanticQuery = serde_json::from_value(args.clone())
                .map_err(|e| ToolError::BadArgs(format!("invalid semantic query: {e}")))?;
            let batches = engine
                .semantic_query_collect(q)
                .await
                .map_err(|e| ToolError::Engine(e.to_string()))?;
            let (columns, rows, total, truncated) = format_batches(&batches, max as usize);
            Ok(json!({
                "columns": columns,
                "rows": rows,
                "row_count": total,
                "truncated": truncated,
            }))
        }
        "materialize" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ToolError::BadArgs("`name` is required".into()))?;
            let format = match args.get("format").and_then(|v| v.as_str()) {
                Some(f) => Some(parse_format(f)?),
                None => None,
            };
            let spec = if let Some(sql) = args.get("sql").and_then(|v| v.as_str()) {
                let params = match args.get("params") {
                    Some(p) => serde_json::from_value(p.clone())
                        .map_err(|e| ToolError::BadArgs(format!("invalid `params`: {e}")))?,
                    None => std::collections::HashMap::new(),
                };
                pawrly_core::MaterializeSpec::Query {
                    sql: sql.to_string(),
                    params,
                }
            } else if let Some(file) = args.get("file").and_then(|v| v.as_str()) {
                pawrly_core::MaterializeSpec::File {
                    path: file.into(),
                    format,
                }
            } else if let Some(url) = args.get("url").and_then(|v| v.as_str()) {
                pawrly_core::MaterializeSpec::Url {
                    url: url.to_string(),
                    format,
                }
            } else {
                return Err(ToolError::BadArgs(
                    "one of `sql`, `file`, or `url` is required".into(),
                ));
            };
            let outcome = engine
                .materialize(name, spec)
                .await
                .map_err(|e| ToolError::Engine(e.to_string()))?;
            serde_json::to_value(&outcome).map_err(|e| ToolError::Engine(e.to_string()))
        }
        "drop_materialized" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ToolError::BadArgs("`name` is required".into()))?;
            let dropped = engine
                .drop_materialized(name)
                .await
                .map_err(|e| ToolError::Engine(e.to_string()))?;
            Ok(json!({ "dropped": dropped }))
        }
        other => Err(ToolError::Unknown(other.to_string())),
    }
}

/// Parse the required `table` argument as a fully-qualified `<schema>.<table>`.
fn table_name_arg(args: &Value) -> Result<pawrly_core::TableName, ToolError> {
    let raw = args
        .get("table")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::BadArgs("`table` is required".into()))?;
    pawrly_core::TableName::parse(raw).ok_or_else(|| {
        ToolError::BadArgs(format!("`table` must be `<schema>.<table>`, got `{raw}`"))
    })
}

/// Parse a `materialize` format string into a `MaterializeFormat`.
fn parse_format(s: &str) -> Result<pawrly_core::MaterializeFormat, ToolError> {
    match s.to_ascii_lowercase().as_str() {
        "parquet" => Ok(pawrly_core::MaterializeFormat::Parquet),
        "csv" => Ok(pawrly_core::MaterializeFormat::Csv),
        "json" | "ndjson" | "jsonl" => Ok(pawrly_core::MaterializeFormat::Json),
        other => Err(ToolError::BadArgs(format!(
            "unknown format `{other}` (expected parquet, csv, or json)"
        ))),
    }
}

fn format_batches(
    batches: &[RecordBatch],
    max: usize,
) -> (Vec<String>, Vec<Vec<Value>>, usize, bool) {
    let columns: Vec<String> = batches
        .first()
        .map(|b| {
            b.schema()
                .fields()
                .iter()
                .map(|f| f.name().clone())
                .collect()
        })
        .unwrap_or_default();
    let opts = FormatOptions::default();
    let mut rows: Vec<Vec<Value>> = Vec::new();
    let mut total = 0usize;
    let mut truncated = false;
    'outer: for batch in batches {
        let formatters: Vec<ArrayFormatter<'_>> = match batch
            .columns()
            .iter()
            .map(|c| ArrayFormatter::try_new(c.as_ref(), &opts))
            .collect::<Result<_, _>>()
        {
            Ok(fs) => fs,
            Err(_) => continue,
        };
        for r in 0..batch.num_rows() {
            if total >= max {
                truncated = true;
                break 'outer;
            }
            let row: Vec<Value> = formatters
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    if batch.column(i).is_null(r) {
                        Value::Null
                    } else {
                        Value::String(format!("{}", f.value(r)))
                    }
                })
                .collect();
            rows.push(row);
            total += 1;
        }
    }
    (columns, rows, total, truncated)
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("engine: {0}")]
    Engine(String),
    #[error("unknown tool `{0}`")]
    Unknown(String),
    #[error("bad arguments: {0}")]
    BadArgs(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use pawrly_core::test_support::MockEngine;

    fn engine() -> Arc<dyn EngineService> {
        Arc::new(MockEngine::new())
    }

    #[test]
    fn semantic_tools_are_listed() {
        let names: Vec<String> = list_tools()
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect();
        for want in [
            "list_semantic_models",
            "describe_semantic_model",
            "semantic_query",
        ] {
            assert!(names.contains(&want.to_string()), "missing tool `{want}`");
        }
    }

    #[tokio::test]
    async fn list_semantic_models_shapes_output() {
        // MockEngine reports no models; the tool still returns the envelope.
        let out = call_tool(&engine(), "list_semantic_models", &json!({}))
            .await
            .unwrap();
        assert!(out["models"].is_array());
    }

    #[tokio::test]
    async fn describe_requires_name() {
        let err = call_tool(&engine(), "describe_semantic_model", &json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::BadArgs(_)));
    }

    #[tokio::test]
    async fn describe_propagates_engine_error() {
        // MockEngine has no models, so describe is an engine error, not a panic.
        let err = call_tool(
            &engine(),
            "describe_semantic_model",
            &json!({ "name": "orders" }),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::Engine(_)));
    }

    #[tokio::test]
    async fn semantic_query_parses_args_and_returns_envelope() {
        let out = call_tool(
            &engine(),
            "semantic_query",
            &json!({
                "measures": ["orders.revenue"],
                "dimensions": ["orders.status"],
                "max_rows": 10
            }),
        )
        .await
        .unwrap();
        assert_eq!(out["row_count"], 0); // MockEngine yields no rows
        assert!(out["columns"].is_array());
    }

    #[test]
    fn materialize_tools_are_listed() {
        let names: Vec<String> = list_tools()
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect();
        for want in ["materialize", "drop_materialized"] {
            assert!(names.contains(&want.to_string()), "missing tool `{want}`");
        }
    }

    #[tokio::test]
    async fn materialize_query_returns_outcome() {
        // MockEngine echoes the name back in the outcome.
        let out = call_tool(
            &engine(),
            "materialize",
            &json!({ "name": "rev", "sql": "SELECT 1" }),
        )
        .await
        .unwrap();
        assert_eq!(out["name"]["schema"], "materialized");
        assert_eq!(out["name"]["table"], "rev");
    }

    #[tokio::test]
    async fn materialize_requires_name_and_origin() {
        // Missing name.
        let err = call_tool(&engine(), "materialize", &json!({ "sql": "SELECT 1" }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::BadArgs(_)));
        // Name but no origin.
        let err = call_tool(&engine(), "materialize", &json!({ "name": "x" }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::BadArgs(_)));
    }

    #[tokio::test]
    async fn materialize_rejects_bad_format() {
        let err = call_tool(
            &engine(),
            "materialize",
            &json!({ "name": "x", "file": "a.dat", "format": "avro" }),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::BadArgs(_)));
    }

    #[tokio::test]
    async fn drop_materialized_returns_envelope() {
        let out = call_tool(&engine(), "drop_materialized", &json!({ "name": "rev" }))
            .await
            .unwrap();
        assert_eq!(out["dropped"], false); // MockEngine reports nothing to drop
    }

    #[test]
    fn catalog_tools_are_listed() {
        let names: Vec<String> = list_tools()
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect();
        for want in [
            "list_sources",
            "describe_table",
            "get_schema",
            "refresh_table",
        ] {
            assert!(names.contains(&want.to_string()), "missing tool `{want}`");
        }
    }

    /// A `MockEngine` with one source and one table registered under it.
    fn populated() -> Arc<dyn EngineService> {
        use pawrly_core::{ColumnSpec, SourceKind, TableName};
        let mock = MockEngine::new();
        mock.add_source("gh", SourceKind::File);
        mock.add_table(
            TableName::new("gh", "pulls"),
            SourceKind::File,
            vec![ColumnSpec {
                name: "number".into(),
                data_type: "Int64".into(),
                nullable: false,
                description: None,
                is_filter_pushable: false,
                is_required_filter: false,
            }],
        );
        Arc::new(mock)
    }

    #[tokio::test]
    async fn list_sources_shapes_output() {
        let out = call_tool(&populated(), "list_sources", &json!({}))
            .await
            .unwrap();
        let sources = out["sources"].as_array().unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0]["name"], "gh");
    }

    #[tokio::test]
    async fn describe_table_returns_columns() {
        let out = call_tool(
            &populated(),
            "describe_table",
            &json!({ "table": "gh.pulls" }),
        )
        .await
        .unwrap();
        assert_eq!(out["table"]["name"]["table"], "pulls");
        assert_eq!(out["columns"][0]["name"], "number");
    }

    #[tokio::test]
    async fn describe_table_rejects_unqualified_name() {
        let err = call_tool(&populated(), "describe_table", &json!({ "table": "pulls" }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::BadArgs(_)));
    }

    #[tokio::test]
    async fn describe_table_requires_table() {
        let err = call_tool(&populated(), "describe_table", &json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::BadArgs(_)));
    }

    #[tokio::test]
    async fn get_schema_returns_snapshot() {
        let out = call_tool(&populated(), "get_schema", &json!({}))
            .await
            .unwrap();
        let schemas = out["schemas"].as_array().unwrap();
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0]["name"], "gh");
    }

    #[tokio::test]
    async fn get_schema_rejects_bad_sources_arg() {
        let err = call_tool(&populated(), "get_schema", &json!({ "sources": "gh" }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::BadArgs(_)));
    }

    #[tokio::test]
    async fn refresh_table_returns_outcome() {
        let out = call_tool(
            &populated(),
            "refresh_table",
            &json!({ "table": "gh.pulls" }),
        )
        .await
        .unwrap();
        assert_eq!(out["table"]["table"], "pulls");
        assert_eq!(out["rows_written"], 0); // MockEngine writes nothing
    }

    #[tokio::test]
    async fn semantic_query_rejects_bad_filter_op() {
        let err = call_tool(
            &engine(),
            "semantic_query",
            &json!({
                "measures": ["orders.revenue"],
                "filters": [{ "member": "orders.status", "op": "not_a_real_op" }]
            }),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::BadArgs(_)));
    }
}
