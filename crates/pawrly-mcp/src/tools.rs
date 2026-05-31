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
        other => Err(ToolError::Unknown(other.to_string())),
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
