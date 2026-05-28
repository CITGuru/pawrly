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
