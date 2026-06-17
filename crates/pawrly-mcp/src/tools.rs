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
            "name": "search_tables",
            "description": "Find tables by keyword across all sources (or one source). \
                            Matches the query terms against table names and descriptions \
                            (case-insensitive; every term must appear), ranked with \
                            name matches ahead of description-only matches. Use this to \
                            discover tables in large catalogs before `describe_table`. \
                            Returns { tables, match_count, truncated }.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Keywords to match against table names and descriptions"
                    },
                    "source": { "type": "string", "description": "Limit to one source" },
                    "limit": {
                        "type": "integer",
                        "default": 50,
                        "description": "Maximum number of matches to return"
                    }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "list_columns",
            "description": "List columns flattened to one row per column \
                            ({ schema, table, column, type, nullable, required_filter, \
                            description }). Scope with `table` (one table), `source` \
                            (one source), and/or `name` (case-insensitive keyword over \
                            column name and description) — use `name` to find which \
                            tables expose a column like `created_at` or `email`. Returns \
                            { columns, column_count, truncated }. Prefer scoping by \
                            `source` or `table` on large catalogs.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "table": {
                        "type": "string",
                        "description": "Limit to one fully-qualified `<schema>.<table>`"
                    },
                    "source": {
                        "type": "string",
                        "description": "Limit to one source (ignored when `table` is given)"
                    },
                    "name": {
                        "type": "string",
                        "description": "Keyword filter over column name and description"
                    },
                    "limit": {
                        "type": "integer",
                        "default": 500,
                        "description": "Maximum number of columns to return"
                    }
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
                    "max_rows": { "type": "integer", "default": 1000 },
                    "query_id": {
                        "type": "string",
                        "description": "Client-chosen id so a concurrent `cancel_query` can abort \
                                        this query (effective over HTTP)."
                    }
                },
                "required": ["sql"]
            }
        }),
        json!({
            "name": "cancel_query",
            "description": "Cancel an in-flight query previously started with a matching \
                            `query_id`. Returns { cancelled } (false if no such query was running).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query_id": { "type": "string" }
                },
                "required": ["query_id"]
            }
        }),
        json!({
            "name": "list_sources",
            "description": "List all configured data sources, their kinds, status, and table counts.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "describe_table",
            "description": "Column schema, descriptions, pushdown affordances, example \
                            queries, and usage notes (`wiki`) for one table. `table` is \
                            fully qualified `<schema>.<table>`.",
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
                    "max_rows": { "type": "integer", "default": 1000 },
                    "query_id": {
                        "type": "string",
                        "description": "Client-chosen id so a concurrent `cancel_query` can abort \
                                        this query (effective over HTTP)."
                    }
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
#[tracing::instrument(name = "pawrly.mcp.tool", skip_all, fields(tool = %name))]
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
        "search_tables" => {
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ToolError::BadArgs("`query` is required".into()))?;
            let terms = search_terms(query);
            if terms.is_empty() {
                return Err(ToolError::BadArgs(
                    "`query` must contain at least one term".into(),
                ));
            }
            let limit = args
                .get("limit")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(50) as usize;
            // Search is built on `list_tables` so it works identically over a
            // local engine or a remote daemon — no extra RPC surface.
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

            let mut scored: Vec<(i32, pawrly_core::TableInfo)> = tables
                .into_iter()
                .filter_map(|t| {
                    let qualified = format!("{}.{}", t.name.schema, t.name.table);
                    let desc = t.description.as_deref().unwrap_or("");
                    table_match_score(&terms, &qualified, &desc.to_lowercase()).map(|s| (s, t))
                })
                .collect();
            // Best score first; ties broken by qualified name for stable output.
            scored.sort_by(|a, b| {
                b.0.cmp(&a.0)
                    .then_with(|| a.1.name.schema.cmp(&b.1.name.schema))
                    .then_with(|| a.1.name.table.cmp(&b.1.name.table))
            });

            let match_count = scored.len();
            let truncated = match_count > limit;
            let rows: Vec<Value> = scored
                .into_iter()
                .take(limit)
                .map(|(score, t)| {
                    json!({
                        "schema": t.name.schema,
                        "name": t.name.table,
                        "kind": t.kind.to_string(),
                        "description": t.description,
                        "required_filters": t.required_filters,
                        "score": score,
                    })
                })
                .collect();
            Ok(json!({
                "tables": rows,
                "match_count": match_count,
                "truncated": truncated,
            }))
        }
        "list_columns" => {
            let name_filter = args
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::to_lowercase);
            let limit = args
                .get("limit")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(500) as usize;

            // Resolve the target tables: one explicit table, or every table in
            // scope (via `list_tables`, so this works on a remote engine too).
            let targets: Vec<pawrly_core::TableName> = if let Some(tbl) =
                args.get("table").and_then(|v| v.as_str())
            {
                vec![pawrly_core::TableName::parse(tbl).ok_or_else(|| {
                    ToolError::BadArgs(format!("`table` must be `<schema>.<table>`, got `{tbl}`"))
                })?]
            } else {
                let filter =
                    args.get("source")
                        .and_then(|v| v.as_str())
                        .map(|s| pawrly_core::TableFilter {
                            source: Some(s.to_string()),
                            name_glob: None,
                        });
                engine
                    .list_tables(filter)
                    .await
                    .map_err(|e| ToolError::Engine(e.to_string()))?
                    .into_iter()
                    .map(|t| t.name)
                    .collect()
            };

            let mut rows: Vec<Value> = Vec::new();
            let mut truncated = false;
            'outer: for table in &targets {
                let desc = engine
                    .describe_table(table)
                    .await
                    .map_err(|e| ToolError::Engine(e.to_string()))?;
                let required: std::collections::HashSet<&str> = desc
                    .table
                    .required_filters
                    .iter()
                    .map(String::as_str)
                    .collect();
                for col in &desc.columns {
                    if let Some(needle) = &name_filter {
                        let in_name = col.name.to_lowercase().contains(needle);
                        let in_desc = col
                            .description
                            .as_deref()
                            .is_some_and(|d| d.to_lowercase().contains(needle));
                        if !in_name && !in_desc {
                            continue;
                        }
                    }
                    if rows.len() >= limit {
                        truncated = true;
                        break 'outer;
                    }
                    rows.push(json!({
                        "schema": table.schema,
                        "table": table.table,
                        "column": col.name,
                        "type": col.data_type,
                        "nullable": col.nullable,
                        "required_filter": required.contains(col.name.as_str()),
                        "description": col.description,
                    }));
                }
            }
            Ok(json!({
                "columns": rows,
                "column_count": rows.len(),
                "truncated": truncated,
            }))
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
            // Push the row bound into the engine so it (and any pushdown-aware
            // source) materializes at most `max + 1` rows instead of the whole
            // result set; the extra row lets `format_batches` report truncation.
            let bounded = row_bounded_sql(sql, max.saturating_add(1));
            let batches = engine
                .query_collect(&bounded)
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

/// Tokenize a search query into distinct lowercased terms.
fn search_terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    terms.dedup();
    terms
}

/// Score one table against the search terms (AND semantics). Returns `None`
/// when any term is absent from both the qualified name and the description.
/// A term hit in the name weighs more than one in the description only, so
/// name matches rank ahead. `qualified` is matched case-insensitively;
/// `desc_lower` must already be lowercased by the caller.
fn table_match_score(terms: &[String], qualified: &str, desc_lower: &str) -> Option<i32> {
    let name_lower = qualified.to_lowercase();
    let mut score = 0i32;
    for term in terms {
        let in_name = name_lower.contains(term);
        let in_desc = desc_lower.contains(term);
        if !in_name && !in_desc {
            return None;
        }
        score += if in_name { 10 } else { 1 };
    }
    Some(score)
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

/// Wrap a read query so the engine returns at most `limit` rows. Only `SELECT`
/// and `WITH` (CTE) statements are wrapped — wrapping preserves any inner
/// `LIMIT`/`ORDER BY` while capping the total — so other statements (`EXPLAIN`,
/// `SHOW`, …) pass through unchanged.
fn row_bounded_sql(sql: &str, limit: u64) -> String {
    let trimmed = sql.trim().trim_end_matches(';').trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("select") || lower.starts_with("with") {
        format!("SELECT * FROM ({trimmed}) AS _pawrly_q LIMIT {limit}")
    } else {
        trimmed.to_string()
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
    #[error("query `{0}` cancelled")]
    Cancelled(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use pawrly_core::test_support::MockEngine;

    fn engine() -> Arc<dyn EngineService> {
        Arc::new(MockEngine::new())
    }

    #[test]
    fn row_bounded_sql_wraps_selects_only() {
        assert_eq!(
            row_bounded_sql("SELECT * FROM t", 100),
            "SELECT * FROM (SELECT * FROM t) AS _pawrly_q LIMIT 100"
        );
        // Trailing semicolon and surrounding whitespace are stripped before wrapping.
        assert_eq!(
            row_bounded_sql("  select a from t ;  ", 5),
            "SELECT * FROM (select a from t) AS _pawrly_q LIMIT 5"
        );
        // CTEs (WITH) are wrapped too.
        assert_eq!(
            row_bounded_sql("WITH x AS (SELECT 1) SELECT * FROM x", 10),
            "SELECT * FROM (WITH x AS (SELECT 1) SELECT * FROM x) AS _pawrly_q LIMIT 10"
        );
        // Non-SELECT statements pass through unchanged.
        assert_eq!(row_bounded_sql("EXPLAIN SELECT 1", 10), "EXPLAIN SELECT 1");
        assert_eq!(row_bounded_sql("SHOW TABLES", 10), "SHOW TABLES");
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

    #[test]
    fn search_tables_is_listed() {
        let names: Vec<String> = list_tools()
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect();
        assert!(names.contains(&"search_tables".to_string()));
    }

    #[test]
    fn table_match_score_requires_all_terms() {
        let terms = vec!["pull".to_string(), "request".to_string()];
        // Both terms present (one in name, one in description).
        assert!(table_match_score(&terms, "gh.pull_requests", "request data").is_some());
        // Second term missing everywhere → no match.
        assert!(table_match_score(&terms, "gh.pulls", "open pulls").is_none());
    }

    #[test]
    fn table_match_score_ranks_name_over_description() {
        let terms = vec!["issue".to_string()];
        let in_name = table_match_score(&terms, "gh.issues", "").unwrap();
        let in_desc = table_match_score(&terms, "gh.tickets", "tracks an issue").unwrap();
        assert!(in_name > in_desc, "name hit should outrank description hit");
    }

    /// A `MockEngine` with a small, searchable catalog.
    fn searchable() -> Arc<dyn EngineService> {
        use pawrly_core::{SourceKind, TableName};
        let mock = MockEngine::new();
        mock.add_source("gh", SourceKind::Http);
        mock.add_table_with_description(
            TableName::new("gh", "issues"),
            SourceKind::Http,
            "Issues opened against a repository",
        );
        mock.add_table_with_description(
            TableName::new("gh", "pull_requests"),
            SourceKind::Http,
            "Pull requests with review state",
        );
        mock.add_table_with_description(
            TableName::new("gh", "commits"),
            SourceKind::Http,
            "Commit history for a branch",
        );
        Arc::new(mock)
    }

    #[tokio::test]
    async fn search_tables_ranks_and_filters() {
        let out = call_tool(&searchable(), "search_tables", &json!({ "query": "issue" }))
            .await
            .unwrap();
        let tables = out["tables"].as_array().unwrap();
        // `issues` (name hit) and `pull_requests` has no "issue"; only `issues` matches.
        assert_eq!(out["match_count"], 1);
        assert_eq!(tables[0]["name"], "issues");
        assert_eq!(out["truncated"], false);
    }

    #[tokio::test]
    async fn search_tables_matches_descriptions() {
        let out = call_tool(
            &searchable(),
            "search_tables",
            &json!({ "query": "review" }),
        )
        .await
        .unwrap();
        let tables = out["tables"].as_array().unwrap();
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0]["name"], "pull_requests");
    }

    #[tokio::test]
    async fn search_tables_honors_limit_and_reports_truncation() {
        // Single common term hits all three tables' source schema prefix `gh`.
        let out = call_tool(
            &searchable(),
            "search_tables",
            &json!({ "query": "gh", "limit": 2 }),
        )
        .await
        .unwrap();
        assert_eq!(out["match_count"], 3);
        assert_eq!(out["truncated"], true);
        assert_eq!(out["tables"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn search_tables_requires_query() {
        let err = call_tool(&searchable(), "search_tables", &json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::BadArgs(_)));
        // Whitespace-only query has no terms.
        let err = call_tool(&searchable(), "search_tables", &json!({ "query": "   " }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::BadArgs(_)));
    }

    #[test]
    fn list_columns_is_listed() {
        let names: Vec<String> = list_tools()
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect();
        assert!(names.contains(&"list_columns".to_string()));
    }

    /// A `MockEngine` with two tables that share a `created_at` column.
    fn with_columns() -> Arc<dyn EngineService> {
        use pawrly_core::{ColumnSpec, SourceKind, TableName};
        let col = |name: &str, ty: &str, desc: Option<&str>| ColumnSpec {
            name: name.into(),
            data_type: ty.into(),
            nullable: true,
            description: desc.map(Into::into),
            is_filter_pushable: false,
            is_required_filter: false,
        };
        let mock = MockEngine::new();
        mock.add_source("gh", SourceKind::Http);
        mock.add_table(
            TableName::new("gh", "issues"),
            SourceKind::Http,
            vec![
                col("number", "Int64", None),
                col("title", "Utf8", Some("Issue title")),
                col("created_at", "Timestamp", Some("When the issue was opened")),
            ],
        );
        mock.add_table(
            TableName::new("gh", "commits"),
            SourceKind::Http,
            vec![
                col("sha", "Utf8", None),
                col("created_at", "Timestamp", Some("Commit timestamp")),
            ],
        );
        Arc::new(mock)
    }

    #[tokio::test]
    async fn list_columns_for_one_table() {
        let out = call_tool(
            &with_columns(),
            "list_columns",
            &json!({ "table": "gh.issues" }),
        )
        .await
        .unwrap();
        assert_eq!(out["column_count"], 3);
        let cols = out["columns"].as_array().unwrap();
        assert_eq!(cols[0]["schema"], "gh");
        assert_eq!(cols[0]["table"], "issues");
        assert_eq!(cols[0]["column"], "number");
        assert_eq!(cols[0]["type"], "Int64");
    }

    #[tokio::test]
    async fn list_columns_name_filter_spans_tables() {
        let out = call_tool(
            &with_columns(),
            "list_columns",
            &json!({ "name": "created_at" }),
        )
        .await
        .unwrap();
        // Both tables carry `created_at`.
        assert_eq!(out["column_count"], 2);
        for c in out["columns"].as_array().unwrap() {
            assert_eq!(c["column"], "created_at");
        }
    }

    #[tokio::test]
    async fn list_columns_name_filter_matches_description() {
        // "opened" appears only in issues.created_at's description.
        let out = call_tool(
            &with_columns(),
            "list_columns",
            &json!({ "name": "opened" }),
        )
        .await
        .unwrap();
        assert_eq!(out["column_count"], 1);
        assert_eq!(out["columns"][0]["table"], "issues");
        assert_eq!(out["columns"][0]["column"], "created_at");
    }

    #[tokio::test]
    async fn list_columns_source_scope() {
        let out = call_tool(&with_columns(), "list_columns", &json!({ "source": "gh" }))
            .await
            .unwrap();
        assert_eq!(out["column_count"], 5); // 3 + 2
    }

    #[tokio::test]
    async fn list_columns_honors_limit() {
        let out = call_tool(
            &with_columns(),
            "list_columns",
            &json!({ "table": "gh.issues", "limit": 2 }),
        )
        .await
        .unwrap();
        assert_eq!(out["column_count"], 2);
        assert_eq!(out["truncated"], true);
    }

    #[tokio::test]
    async fn list_columns_rejects_unqualified_table() {
        let err = call_tool(
            &with_columns(),
            "list_columns",
            &json!({ "table": "issues" }),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::BadArgs(_)));
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
