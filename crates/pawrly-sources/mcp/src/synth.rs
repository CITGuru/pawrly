//! Turn discovered tools into table specs, gate them by `expose`, and apply the
//! declarative `tables:` patch-or-define layer.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::session::Tool;

/// A diagnostic surfaced during synthesis (held-back tool, missing declaration).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub message: String,
    pub table: Option<String>,
}

/// How much introspection auto-exposes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Expose {
    #[default]
    ReadOnly,
    All,
    Listed,
}

#[derive(Debug, Clone, Default)]
pub struct SynthOptions {
    pub expose: Expose,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

/// A SQL-filterable input that binds into a tool argument (and is echoed as a
/// column so `WHERE arg = …` resolves).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Arg {
    pub name: String,
    #[serde(default = "default_type")]
    pub r#type: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_arg: Option<String>,
}

impl Arg {
    pub fn wire_name(&self) -> &str {
        self.tool_arg.as_deref().unwrap_or(&self.name)
    }
}

/// One output column, pulled from the row element at `path` (empty `path` = the
/// whole element, as JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    #[serde(default = "default_type")]
    pub r#type: String,
    #[serde(default)]
    pub path: Vec<String>,
}

/// Push SQL `LIMIT` into a tool argument.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitBinding {
    pub tool_arg: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<usize>,
}

/// Cursor pagination: send the cursor as `cursor_arg`, read the next cursor from
/// the result at `response_cursor_path`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pagination {
    pub cursor_arg: String,
    pub response_cursor_path: Vec<String>,
}

/// One MCP-backed SQL table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTableSpec {
    pub name: String,
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub args: Vec<Arg>,
    #[serde(default)]
    pub columns: Vec<Column>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_args: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_binding: Option<LimitBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pagination: Option<Pagination>,
    /// Path into the tool result's `structuredContent` to the rows array; empty
    /// means classify the structured content at scan time.
    #[serde(default)]
    pub rows_path: Vec<String>,
}

fn default_type() -> String {
    "varchar".into()
}

#[derive(Debug, Default)]
pub struct Synthesis {
    pub tables: Vec<McpTableSpec>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Build table specs from discovered tools, gated by `expose`/`include`/`exclude`.
pub fn synthesize_tools(tools: &[Tool], opts: &SynthOptions) -> Synthesis {
    let mut out = Synthesis::default();
    for tool in tools {
        if opts.exclude.iter().any(|n| n == &tool.name) {
            continue;
        }
        if !opts.include.is_empty() && !opts.include.iter().any(|n| n == &tool.name) {
            continue;
        }
        if !admit(tool, opts.expose) {
            out.diagnostics.push(Diagnostic {
                code: "MCP_TOOL_HELD_BACK",
                message: format!(
                    "tool `{}` is not read-only; set `expose: all` or list it under `include`",
                    tool.name
                ),
                table: Some(tool.name.clone()),
            });
            continue;
        }
        out.tables.push(table_from_tool(tool));
    }
    out.tables.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Whether a tool is admitted for auto-exposure under `expose`.
fn admit(tool: &Tool, expose: Expose) -> bool {
    if tool.destructive == Some(true) {
        return false;
    }
    match expose {
        Expose::ReadOnly => tool.read_only == Some(true),
        Expose::All => true,
        Expose::Listed => false,
    }
}

fn table_from_tool(tool: &Tool) -> McpTableSpec {
    let args = tool_args(&tool.input_schema);
    let limit_binding = infer_limit_binding(&args);
    let pagination = infer_pagination(&args);
    let (rows_path, columns) = match &tool.output_schema {
        Some(schema) => output_columns(schema),
        None => (Vec::new(), vec![result_column()]),
    };
    McpTableSpec {
        name: sanitize(&tool.name),
        tool: tool.name.clone(),
        description: tool.description.clone(),
        args,
        columns,
        tool_args: BTreeMap::new(),
        limit_binding,
        pagination,
        rows_path,
    }
}

/// Derive output columns + rows-path from a tool's `outputSchema` using the
/// shared classifier; fall back to a single `result` json column.
fn output_columns(output_schema: &Value) -> (Vec<String>, Vec<Column>) {
    let schema = pawrly_schema::deref(output_schema, output_schema);
    if pawrly_schema::schema_type(&schema).as_deref() == Some("array") {
        let item = deref_field(output_schema, &schema, "items");
        return (Vec::new(), columns_of(output_schema, &item));
    }
    if pawrly_schema::is_object(&schema)
        && let Some(props) = schema.get("properties").and_then(Value::as_object)
    {
        if let Some((prop, array)) = pawrly_schema::rows_array(output_schema, props) {
            let item = deref_field(output_schema, &array, "items");
            return (vec![prop], columns_of(output_schema, &item));
        }
        return (Vec::new(), columns_of(output_schema, &schema));
    }
    (Vec::new(), vec![result_column()])
}

fn columns_of(doc: &Value, item: &Value) -> Vec<Column> {
    let Some(props) = item.get("properties").and_then(Value::as_object) else {
        return vec![result_column()];
    };
    let mut columns: Vec<Column> = props
        .iter()
        .map(|(name, schema)| Column {
            name: name.clone(),
            r#type: pawrly_schema::column_type(&pawrly_schema::deref(doc, schema)),
            path: vec![name.clone()],
        })
        .collect();
    columns.sort_by(|a, b| a.name.cmp(&b.name));
    columns
}

fn deref_field(doc: &Value, schema: &Value, field: &str) -> Value {
    schema
        .get(field)
        .map(|v| pawrly_schema::deref(doc, v))
        .unwrap_or(Value::Null)
}

fn result_column() -> Column {
    Column {
        name: "result".into(),
        r#type: "json".into(),
        path: Vec::new(),
    }
}

/// One arg per `inputSchema` property (path-and-query-free; all are arguments).
fn tool_args(input_schema: &Value) -> Vec<Arg> {
    let Some(props) = input_schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    let required: Vec<&str> = input_schema
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut args: Vec<Arg> = props
        .iter()
        .map(|(name, schema)| Arg {
            name: name.clone(),
            r#type: pawrly_schema::param_type(schema),
            required: required.contains(&name.as_str()),
            tool_arg: None,
        })
        .collect();
    args.sort_by(|a, b| a.name.cmp(&b.name));
    args
}

fn infer_limit_binding(args: &[Arg]) -> Option<LimitBinding> {
    let name = ["limit", "first", "page_size", "pageSize", "maxResults"]
        .into_iter()
        .find(|n| args.iter().any(|a| a.name == *n))?;
    Some(LimitBinding {
        tool_arg: name.to_string(),
        max: None,
    })
}

fn infer_pagination(args: &[Arg]) -> Option<Pagination> {
    let name = ["cursor", "after", "startCursor", "page_token"]
        .into_iter()
        .find(|n| args.iter().any(|a| a.name == *n))?;
    Some(Pagination {
        cursor_arg: name.to_string(),
        response_cursor_path: vec!["nextCursor".to_string()],
    })
}

/// MCP tool names are usually identifiers already; keep `[a-z0-9_]`, lowercase.
fn sanitize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "tool".into()
    } else {
        trimmed
    }
}

/// Apply a `tables:` entry: patch a synthesized table of the same name, or define
/// a new tool-backed table. `tool_names` is the set advertised by the server, for
/// validating declared `tool:`s.
pub fn apply_table_def(
    tables: &mut Vec<McpTableSpec>,
    name: &str,
    description: Option<&str>,
    body: &Value,
    tool_names: &[String],
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(), String> {
    let mut patch = body.clone();
    if let Some(map) = patch.as_object_mut() {
        map.remove("name");
        if let Some(desc) = description {
            map.entry("description")
                .or_insert_with(|| Value::String(desc.to_string()));
        }
    }

    if let Some(existing) = tables.iter_mut().find(|t| t.name == name) {
        let mut value = serde_json::to_value(&*existing).map_err(|e| e.to_string())?;
        pawrly_schema::deep_merge(&mut value, &patch);
        *existing = serde_json::from_value(value).map_err(|e| e.to_string())?;
    } else {
        // New table: requires a `tool`.
        let mut value = serde_json::json!({ "name": name });
        pawrly_schema::deep_merge(&mut value, &patch);
        if value.get("tool").and_then(Value::as_str).is_none() {
            return Err(format!(
                "table `{name}` does not match a synthesized tool and declares no `tool:`"
            ));
        }
        let spec: McpTableSpec = serde_json::from_value(value).map_err(|e| e.to_string())?;
        if !tool_names.iter().any(|t| t == &spec.tool) {
            diagnostics.push(Diagnostic {
                code: "MCP_DECLARED_TOOL_ABSENT",
                message: format!(
                    "declared tool `{}` is not advertised by the server",
                    spec.tool
                ),
                table: Some(name.to_string()),
            });
        }
        tables.push(spec);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str, read_only: Option<bool>) -> Tool {
        Tool {
            name: name.into(),
            description: None,
            input_schema: json!({
                "type": "object",
                "properties": { "query": { "type": "string" }, "limit": { "type": "integer" }, "after": { "type": "string" } },
                "required": ["query"]
            }),
            output_schema: None,
            read_only,
            destructive: None,
        }
    }

    #[test]
    fn synthesizes_args_limit_and_pagination() {
        let s = synthesize_tools(&[tool("search", Some(true))], &SynthOptions::default());
        assert_eq!(s.tables.len(), 1);
        let t = &s.tables[0];
        assert_eq!(t.tool, "search");
        assert!(t.args.iter().any(|a| a.name == "query" && a.required));
        assert_eq!(t.limit_binding.as_ref().unwrap().tool_arg, "limit");
        assert_eq!(t.pagination.as_ref().unwrap().cursor_arg, "after");
        assert_eq!(t.columns[0].name, "result");
    }

    #[test]
    fn expose_gates_non_read_only() {
        let tools = [
            tool("search", Some(true)),
            tool("write", Some(false)),
            tool("unknown", None),
        ];
        let read_only = synthesize_tools(
            &tools,
            &SynthOptions {
                expose: Expose::ReadOnly,
                ..Default::default()
            },
        );
        assert_eq!(
            read_only
                .tables
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            ["search"]
        );
        assert_eq!(
            read_only
                .diagnostics
                .iter()
                .filter(|d| d.code == "MCP_TOOL_HELD_BACK")
                .count(),
            2
        );

        let all = synthesize_tools(
            &tools,
            &SynthOptions {
                expose: Expose::All,
                ..Default::default()
            },
        );
        // `write`/`unknown` admitted, but a destructive-hinted tool never would be.
        assert_eq!(all.tables.len(), 3);

        let listed = synthesize_tools(
            &tools,
            &SynthOptions {
                expose: Expose::Listed,
                ..Default::default()
            },
        );
        assert!(listed.tables.is_empty());
    }

    #[test]
    fn table_def_patches_and_defines() {
        let mut tables =
            synthesize_tools(&[tool("search", Some(true))], &SynthOptions::default()).tables;
        let mut diags = Vec::new();

        // Patch: change pagination on the synthesized table.
        apply_table_def(
            &mut tables,
            "search",
            None,
            &json!({ "pagination": { "cursor_arg": "cursor", "response_cursor_path": ["meta", "next"] } }),
            &["search".into()],
            &mut diags,
        )
        .unwrap();
        let patched = tables.iter().find(|t| t.name == "search").unwrap();
        assert_eq!(patched.pagination.as_ref().unwrap().cursor_arg, "cursor");
        assert_eq!(patched.tool, "search"); // tool kept

        // Define a new table backed by a known tool.
        apply_table_def(
            &mut tables,
            "open_prs",
            None,
            &json!({ "tool": "search", "tool_args": { "state": "open" } }),
            &["search".into()],
            &mut diags,
        )
        .unwrap();
        assert!(
            tables
                .iter()
                .any(|t| t.name == "open_prs" && t.tool == "search")
        );

        // Define without a tool → error.
        let err =
            apply_table_def(&mut tables, "bad", None, &json!({}), &[], &mut diags).unwrap_err();
        assert!(err.contains("declares no `tool:`"));
    }
}
