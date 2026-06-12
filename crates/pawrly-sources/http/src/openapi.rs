//! Synthesize HTTP tables from an OpenAPI 3.0.x document.
//!
//! Each GET operation becomes one [`HttpTableSpec`]: parameters map to
//! [`ParamSpec`]s, the JSON response schema maps to columns and a rows path, and
//! pagination is inferred from generic conventions. The document is walked as a
//! raw [`serde_json::Value`] rather than a typed model so that real-world specs
//! (which routinely violate strict schemas) still synthesize. Problems are
//! collected as [`Diagnostic`]s instead of aborting the whole source.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde_json::Value;

use pawrly_schema::{
    column_type as map_column_type, deref, is_object, param_type as map_param_type,
    rows_array as wrapped_list, schema_type,
};

use crate::source::{HttpTableSpec, PaginationConfig, ParamSpec, ResponseColumn, ResponseSpec};

/// A non-fatal problem encountered while synthesizing a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub message: String,
    pub table: Option<String>,
}

impl Diagnostic {
    fn new(code: &'static str, table: Option<&str>, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            table: table.map(str::to_string),
        }
    }
}

/// Which operations become tables. An empty selector includes everything.
#[derive(Debug, Clone, Default)]
pub struct Selector {
    pub tags: Vec<String>,
    pub paths: Vec<String>,
    pub operations: Vec<String>,
}

impl Selector {
    fn is_empty(&self) -> bool {
        self.tags.is_empty() && self.paths.is_empty() && self.operations.is_empty()
    }

    fn matches(&self, path: &str, op: &Value) -> bool {
        if self
            .operations
            .iter()
            .any(|id| operation_id(op) == Some(id.as_str()))
        {
            return true;
        }
        if self.paths.iter().any(|glob| glob_match(glob, path)) {
            return true;
        }
        let tags = op.get("tags").and_then(Value::as_array);
        self.tags.iter().any(|want| {
            tags.is_some_and(|tags| tags.iter().filter_map(Value::as_str).any(|t| t == want))
        })
    }
}

/// How a table is named from its operation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Naming {
    #[default]
    OperationId,
    Path,
    Tag,
}

#[derive(Debug, Clone, Default)]
pub struct SynthOptions {
    pub include: Selector,
    pub exclude: Selector,
    pub naming: Naming,
}

#[derive(Debug, Default)]
pub struct Synthesis {
    /// Effective request base from `servers[0].url`, if the document declares one.
    pub base_url: Option<String>,
    pub tables: Vec<HttpTableSpec>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, thiserror::Error)]
pub enum SynthError {
    #[error("OpenAPI document is missing the `openapi` version field")]
    MissingVersion,
    #[error("unsupported OpenAPI version `{0}` (only 3.0.x is supported)")]
    UnsupportedVersion(String),
    #[error("OpenAPI document is missing `paths`")]
    MissingPaths,
}

/// Synthesize tables from a parsed OpenAPI document.
pub fn synthesize(doc: &Value, opts: &SynthOptions) -> Result<Synthesis, SynthError> {
    let version = doc
        .get("openapi")
        .and_then(Value::as_str)
        .ok_or(SynthError::MissingVersion)?;
    if !version.starts_with("3.0.") {
        return Err(SynthError::UnsupportedVersion(version.to_string()));
    }
    let paths = doc
        .get("paths")
        .and_then(Value::as_object)
        .ok_or(SynthError::MissingPaths)?;

    let mut out = Synthesis {
        base_url: server_url(doc),
        ..Default::default()
    };
    let mut taken: HashSet<String> = HashSet::new();

    for (path, item) in paths {
        let Some(item) = item.as_object() else {
            continue;
        };
        let Some(op) = item.get("get").and_then(Value::as_object) else {
            continue;
        };
        let op = Value::Object(op.clone());

        let included = opts.include.is_empty() || opts.include.matches(path, &op);
        let excluded = !opts.exclude.is_empty() && opts.exclude.matches(path, &op);
        if !included || excluded {
            continue;
        }

        let name = unique_name(table_name(&op, path, opts.naming), &mut taken);
        let path_params = item.get("parameters");
        let params = build_params(doc, path_params, &op, &name, &mut out.diagnostics);
        let response = build_response(doc, &op, &name, &mut out.diagnostics);
        let pagination = infer_pagination(&params, &response_fields(doc, &op));

        out.tables.push(HttpTableSpec {
            name,
            endpoint: path.clone(),
            method: "GET".into(),
            params,
            headers: BTreeMap::new(),
            body: None,
            requests: Vec::new(),
            response,
            pagination,
            description: operation_description(&op),
        });
    }

    out.tables.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn operation_id(op: &Value) -> Option<&str> {
    op.get("operationId").and_then(Value::as_str)
}

fn operation_description(op: &Value) -> Option<String> {
    op.get("summary")
        .or_else(|| op.get("description"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn server_url(doc: &Value) -> Option<String> {
    let server = doc
        .get("servers")
        .and_then(Value::as_array)?
        .iter()
        .find_map(|s| s.get("url").and_then(Value::as_str))?;
    Some(resolve_server_template(doc, server))
}

/// Substitute `{var}` in a server URL with each variable's declared `default`.
fn resolve_server_template(doc: &Value, url: &str) -> String {
    if !url.contains('{') {
        return url.to_string();
    }
    let variables = doc
        .get("servers")
        .and_then(Value::as_array)
        .and_then(|s| s.first())
        .and_then(|s| s.get("variables"))
        .and_then(Value::as_object);
    let mut out = String::with_capacity(url.len());
    let mut rest = url;
    while let Some((before, after_open)) = rest.split_once('{') {
        out.push_str(before);
        let Some((name, after_close)) = after_open.split_once('}') else {
            out.push('{');
            rest = after_open;
            continue;
        };
        let default = variables
            .and_then(|v| v.get(name))
            .and_then(|v| v.get("default"))
            .and_then(Value::as_str)
            .unwrap_or("");
        out.push_str(default);
        rest = after_close;
    }
    out.push_str(rest);
    out
}

fn table_name(op: &Value, path: &str, naming: Naming) -> String {
    let raw = match naming {
        Naming::OperationId => operation_id(op)
            .map(to_snake)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| path_name(path)),
        Naming::Path => path_name(path),
        Naming::Tag => {
            let tag = op
                .get("tags")
                .and_then(Value::as_array)
                .and_then(|t| t.first())
                .and_then(Value::as_str)
                .map(to_snake)
                .filter(|s| !s.is_empty());
            match tag {
                Some(tag) => format!("{tag}_{}", path_leaf(path)),
                None => path_name(path),
            }
        }
    };
    if raw.is_empty() { "table".into() } else { raw }
}

/// Join a path's non-parameter segments into a snake-case identifier.
fn path_name(path: &str) -> String {
    let joined: Vec<String> = path
        .split('/')
        .filter(|s| !s.is_empty() && !s.starts_with('{'))
        .map(to_snake)
        .filter(|s| !s.is_empty())
        .collect();
    joined.join("_")
}

/// The last non-parameter path segment, snake-cased.
fn path_leaf(path: &str) -> String {
    path.split('/')
        .rfind(|s| !s.is_empty() && !s.starts_with('{'))
        .map(to_snake)
        .unwrap_or_default()
}

/// Lowercase snake_case: insert `_` at camelCase boundaries, fold any run of
/// non-alphanumeric characters to a single `_`, and trim leading/trailing `_`.
fn to_snake(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 4);
    let mut prev_lower_or_digit = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            if ch.is_ascii_uppercase() && prev_lower_or_digit {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
            prev_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        } else {
            if !out.ends_with('_') {
                out.push('_');
            }
            prev_lower_or_digit = false;
        }
    }
    out.trim_matches('_').to_string()
}

fn unique_name(base: String, taken: &mut HashSet<String>) -> String {
    if taken.insert(base.clone()) {
        return base;
    }
    for n in 2.. {
        let candidate = format!("{base}_{n}");
        if taken.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("name suffix space is unbounded")
}

fn build_params(
    doc: &Value,
    path_level: Option<&Value>,
    op: &Value,
    table: &str,
    diags: &mut Vec<Diagnostic>,
) -> Vec<ParamSpec> {
    let mut merged: BTreeMap<(String, String), Value> = BTreeMap::new();
    let path_params = path_level.and_then(Value::as_array).into_iter().flatten();
    let op_params = op
        .get("parameters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();

    for param in path_params.chain(op_params) {
        let resolved = deref(doc, param);
        let Some(obj) = resolved.as_object() else {
            diags.push(Diagnostic::new(
                "OPENAPI_PARAM_INVALID",
                Some(table),
                "parameter is not an object",
            ));
            continue;
        };
        let Some(name) = obj.get("name").and_then(Value::as_str) else {
            diags.push(Diagnostic::new(
                "OPENAPI_PARAM_INVALID",
                Some(table),
                "parameter has no name",
            ));
            continue;
        };
        let Some(location) = obj.get("in").and_then(Value::as_str) else {
            continue;
        };
        if location != "path" && location != "query" {
            diags.push(Diagnostic::new(
                "OPENAPI_PARAM_LOCATION_SKIPPED",
                Some(table),
                format!("`{location}` parameter `{name}` is not pushed down"),
            ));
            continue;
        }
        merged.insert((location.to_string(), name.to_string()), resolved.clone());
    }

    merged
        .into_iter()
        .map(|((location, name), param)| {
            let schema = param.get("schema").map(|s| deref(doc, s));
            let required = location == "path"
                || param
                    .get("required")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
            ParamSpec {
                name,
                r#type: schema
                    .as_ref()
                    .map(map_param_type)
                    .unwrap_or_else(|| "varchar".into()),
                required,
                default: schema
                    .as_ref()
                    .and_then(|s| s.get("default"))
                    .map(scalar_to_string),
                accepts: Vec::new(),
                emit: BTreeMap::new(),
                explode: false,
            }
        })
        .collect()
}

fn build_response(
    doc: &Value,
    op: &Value,
    table: &str,
    diags: &mut Vec<Diagnostic>,
) -> ResponseSpec {
    let Some(schema) = success_schema(doc, op) else {
        diags.push(Diagnostic::new(
            "OPENAPI_NO_JSON_RESPONSE",
            Some(table),
            "no 2xx application/json response; exposing a single json column",
        ));
        return single_json_response();
    };
    let schema = deref(doc, &schema);
    let (path, columns) = classify(doc, &schema, table, diags);
    ResponseSpec {
        path,
        schema: columns,
        allow_404_empty: false,
        error: None,
    }
}

/// Pick the success response schema: prefer `200`, then the lowest 2xx, then a
/// `2XX` range; only `application/json` content is considered.
fn success_schema(doc: &Value, op: &Value) -> Option<Value> {
    let responses = op.get("responses").and_then(Value::as_object)?;
    let mut numeric: Vec<(u16, Value)> = Vec::new();
    let mut range: Option<Value> = None;
    for (status, response) in responses {
        let resolved = deref(doc, response);
        let Some(schema) = resolved
            .get("content")
            .and_then(|c| c.get("application/json"))
            .and_then(|j| j.get("schema"))
        else {
            continue;
        };
        if let Ok(code) = status.parse::<u16>() {
            if (200..300).contains(&code) {
                numeric.push((code, schema.clone()));
            }
        } else if status.eq_ignore_ascii_case("2XX") {
            range = Some(schema.clone());
        }
    }
    numeric
        .iter()
        .find(|(c, _)| *c == 200)
        .or_else(|| numeric.iter().min_by_key(|(c, _)| *c))
        .map(|(_, s)| s.clone())
        .or(range)
}

/// Return the rows path and columns for a (already dereferenced) response schema.
fn classify(
    doc: &Value,
    schema: &Value,
    table: &str,
    diags: &mut Vec<Diagnostic>,
) -> (String, Vec<ResponseColumn>) {
    if schema_type(schema).as_deref() == Some("array") {
        let item = schema
            .get("items")
            .map(|i| deref(doc, i))
            .unwrap_or(Value::Null);
        return ("$".into(), object_columns(doc, &item, table, diags));
    }
    if is_object(schema) {
        if let Some(props) = schema.get("properties").and_then(Value::as_object) {
            if let Some((prop, array)) = wrapped_list(doc, props) {
                let item = array
                    .get("items")
                    .map(|i| deref(doc, i))
                    .unwrap_or(Value::Null);
                return (
                    format!("$.{prop}"),
                    object_columns(doc, &item, table, diags),
                );
            }
        }
        return ("$".into(), object_columns(doc, schema, table, diags));
    }
    diags.push(Diagnostic::new(
        "OPENAPI_RESPONSE_UNCLASSIFIED",
        Some(table),
        "response schema is neither array nor object; exposing a single json column",
    ));
    ("$".into(), vec![json_column("value")])
}

fn object_columns(
    doc: &Value,
    schema: &Value,
    table: &str,
    diags: &mut Vec<Diagnostic>,
) -> Vec<ResponseColumn> {
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        diags.push(Diagnostic::new(
            "OPENAPI_ROW_SCHEMA_OPAQUE",
            Some(table),
            "row schema declares no properties (e.g. polymorphic anyOf); exposing a single json column",
        ));
        return vec![json_column("value")];
    };
    let mut columns: Vec<ResponseColumn> = props
        .iter()
        .map(|(name, prop)| ResponseColumn {
            name: name.clone(),
            r#type: map_column_type(&deref(doc, prop)),
            source: None,
        })
        .collect();
    columns.sort_by(|a, b| a.name.cmp(&b.name));
    columns
}

fn json_column(name: &str) -> ResponseColumn {
    ResponseColumn {
        name: name.to_string(),
        r#type: "json".into(),
        source: None,
    }
}

fn single_json_response() -> ResponseSpec {
    ResponseSpec {
        path: "$".into(),
        schema: vec![json_column("value")],
        allow_404_empty: false,
        error: None,
    }
}

/// The set of top-level property names on the success response envelope, used to
/// detect pagination continuation signals.
fn response_fields(doc: &Value, op: &Value) -> BTreeSet<String> {
    let Some(schema) = success_schema(doc, op) else {
        return BTreeSet::new();
    };
    deref(doc, &schema)
        .get("properties")
        .and_then(Value::as_object)
        .map(|props| props.keys().cloned().collect())
        .unwrap_or_default()
}

/// Infer pagination from generic conventions: page/per_page, offset/limit, a
/// token cursor backed by a next-cursor field, or a last-row cursor
/// (`starting_after` + a `has_more` flag).
fn infer_pagination(
    params: &[ParamSpec],
    response_fields: &BTreeSet<String>,
) -> Option<PaginationConfig> {
    let has = |name: &str| params.iter().any(|p| p.name == name);
    let first = |names: &[&str]| names.iter().copied().find(|n| has(n)).map(str::to_string);

    if has("page") {
        if let Some(size_param) = first(&["per_page", "page_size", "pageSize", "limit"]) {
            return Some(PaginationConfig::Page {
                param: "page".into(),
                start: 1,
                size_param: Some(size_param),
                size: None,
            });
        }
    }
    if let Some(offset) = first(&["offset", "skip"]) {
        if has("limit") {
            return Some(PaginationConfig::Offset {
                param: offset,
                size_param: "limit".into(),
                size: 100,
            });
        }
    }
    if let Some(param) = first(&["cursor", "page_token", "next_token"]) {
        if let Some(field) = ["next_cursor", "next_page_token", "next"]
            .iter()
            .find(|f| response_fields.contains(**f))
        {
            return Some(PaginationConfig::Cursor {
                next_path: format!("$.{field}"),
                param,
            });
        }
    }
    if let Some(param) = first(&["starting_after", "after"]) {
        if let Some(flag) = ["has_more", "hasMore"]
            .iter()
            .find(|f| response_fields.contains(**f))
        {
            return Some(PaginationConfig::RowCursor {
                param,
                field: "id".into(),
                more_path: Some(format!("$.{flag}")),
            });
        }
    }
    None
}

fn scalar_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn glob_match(pattern: &str, text: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == text;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut pos = 0usize;
    let last = parts.len() - 1;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            if !text[pos..].starts_with(part) {
                return false;
            }
            pos += part.len();
        } else if i == last {
            return text.len() >= pos + part.len() && text[pos..].ends_with(part);
        } else {
            match text[pos..].find(part) {
                Some(idx) => pos += idx + part.len(),
                None => return false,
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn opts() -> SynthOptions {
        SynthOptions::default()
    }

    fn synth(doc: &Value) -> Synthesis {
        synthesize(doc, &opts()).expect("synthesize")
    }

    fn table<'a>(s: &'a Synthesis, name: &str) -> &'a HttpTableSpec {
        s.tables.iter().find(|t| t.name == name).expect("table")
    }

    #[test]
    fn rejects_non_3_0_versions() {
        assert!(matches!(
            synthesize(&json!({ "openapi": "3.1.0", "paths": {} }), &opts()),
            Err(SynthError::UnsupportedVersion(_))
        ));
        assert!(matches!(
            synthesize(&json!({ "swagger": "2.0", "paths": {} }), &opts()),
            Err(SynthError::MissingVersion)
        ));
        assert!(matches!(
            synthesize(&json!({ "openapi": "3.0.3" }), &opts()),
            Err(SynthError::MissingPaths)
        ));
    }

    #[test]
    fn maps_get_operation_to_table() {
        let doc = json!({
            "openapi": "3.0.0",
            "servers": [{ "url": "https://api.example.com" }],
            "paths": {
                "/repos/{owner}/{repo}/pulls": {
                    "get": {
                        "operationId": "listPulls",
                        "summary": "List pull requests",
                        "parameters": [
                            { "name": "owner", "in": "path", "required": true, "schema": { "type": "string" } },
                            { "name": "repo", "in": "path", "required": true, "schema": { "type": "string" } },
                            { "name": "state", "in": "query", "schema": { "type": "string", "default": "open" } }
                        ],
                        "responses": {
                            "200": {
                                "content": { "application/json": { "schema": {
                                    "type": "array",
                                    "items": { "type": "object", "properties": {
                                        "number": { "type": "integer", "format": "int64" },
                                        "title": { "type": "string" }
                                    }}
                                }}}
                            }
                        }
                    }
                }
            }
        });
        let s = synth(&doc);
        assert_eq!(s.base_url.as_deref(), Some("https://api.example.com"));
        let t = table(&s, "list_pulls");
        assert_eq!(t.endpoint, "/repos/{owner}/{repo}/pulls");
        assert_eq!(t.method, "GET");
        assert_eq!(t.description.as_deref(), Some("List pull requests"));
        assert_eq!(t.response.path, "$");

        let owner = t.params.iter().find(|p| p.name == "owner").unwrap();
        assert!(owner.required);
        let state = t.params.iter().find(|p| p.name == "state").unwrap();
        assert!(!state.required);
        assert_eq!(state.default.as_deref(), Some("open"));

        let cols: BTreeMap<_, _> = t
            .response
            .schema
            .iter()
            .map(|c| (c.name.as_str(), c.r#type.as_str()))
            .collect();
        assert_eq!(cols["number"], "bigint");
        assert_eq!(cols["title"], "varchar");
    }

    #[test]
    fn non_get_methods_are_ignored() {
        let doc = json!({
            "openapi": "3.0.0",
            "paths": { "/things": {
                "post": { "operationId": "createThing", "responses": {} },
                "delete": { "operationId": "deleteThing", "responses": {} }
            }}
        });
        assert!(synth(&doc).tables.is_empty());
    }

    #[test]
    fn resolves_local_refs() {
        let doc = json!({
            "openapi": "3.0.0",
            "paths": { "/users": { "get": {
                "operationId": "listUsers",
                "responses": { "200": { "content": { "application/json": { "schema": {
                    "$ref": "#/components/schemas/UserList"
                }}}}}
            }}},
            "components": { "schemas": {
                "UserList": { "type": "object", "properties": {
                    "data": { "type": "array", "items": { "$ref": "#/components/schemas/User" } }
                }},
                "User": { "type": "object", "properties": {
                    "id": { "type": "integer" },
                    "email": { "type": "string" }
                }}
            }}
        });
        let s = synth(&doc);
        let t = table(&s, "list_users");
        assert_eq!(t.response.path, "$.data");
        let names: Vec<_> = t.response.schema.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["email", "id"]);
    }

    #[test]
    fn classifies_wrapped_list_by_metadata_skip() {
        // Two arrays, but `has_more` style metadata is skipped, leaving one payload array.
        let schema = json!({
            "type": "object",
            "properties": {
                "records": { "type": "array", "items": { "type": "object", "properties": { "id": { "type": "integer" } } } },
                "has_more": { "type": "boolean" }
            }
        });
        let mut diags = Vec::new();
        let (path, cols) = classify(&Value::Null, &schema, "t", &mut diags);
        assert_eq!(path, "$.records");
        assert_eq!(cols.len(), 1);
        assert!(diags.is_empty());
    }

    #[test]
    fn singleton_object_is_root_path() {
        let schema = json!({
            "type": "object",
            "properties": { "id": { "type": "integer" }, "name": { "type": "string" } }
        });
        let mut diags = Vec::new();
        let (path, cols) = classify(&Value::Null, &schema, "t", &mut diags);
        assert_eq!(path, "$");
        assert_eq!(cols.len(), 2);
    }

    #[test]
    fn nested_objects_become_json_columns() {
        let schema = json!({
            "type": "object",
            "properties": {
                "id": { "type": "integer" },
                "owner": { "type": "object", "properties": { "login": { "type": "string" } } },
                "labels": { "type": "array", "items": { "type": "string" } }
            }
        });
        let cols = object_columns(&Value::Null, &schema, "t", &mut Vec::new());
        let by: BTreeMap<_, _> = cols
            .iter()
            .map(|c| (c.name.as_str(), c.r#type.as_str()))
            .collect();
        assert_eq!(by["owner"], "json");
        assert_eq!(by["labels"], "json");
        assert_eq!(by["id"], "bigint");
    }

    #[test]
    fn infers_page_pagination() {
        let params = vec![
            ParamSpec {
                name: "page".into(),
                r#type: "int".into(),
                required: false,
                default: None,
                accepts: vec![],
                emit: BTreeMap::new(),
                explode: false,
            },
            ParamSpec {
                name: "per_page".into(),
                r#type: "int".into(),
                required: false,
                default: None,
                accepts: vec![],
                emit: BTreeMap::new(),
                explode: false,
            },
        ];
        let pg = infer_pagination(&params, &BTreeSet::new());
        assert!(
            matches!(pg, Some(PaginationConfig::Page { ref param, ref size_param, .. }) if param == "page" && size_param.as_deref() == Some("per_page"))
        );
    }

    #[test]
    fn infers_offset_pagination() {
        let params = vec![
            ParamSpec {
                name: "offset".into(),
                r#type: "int".into(),
                required: false,
                default: None,
                accepts: vec![],
                emit: BTreeMap::new(),
                explode: false,
            },
            ParamSpec {
                name: "limit".into(),
                r#type: "int".into(),
                required: false,
                default: None,
                accepts: vec![],
                emit: BTreeMap::new(),
                explode: false,
            },
        ];
        assert!(matches!(
            infer_pagination(&params, &BTreeSet::new()),
            Some(PaginationConfig::Offset { .. })
        ));
    }

    #[test]
    fn infers_token_cursor_only_with_response_field() {
        let params = vec![ParamSpec {
            name: "page_token".into(),
            r#type: "varchar".into(),
            required: false,
            default: None,
            accepts: vec![],
            emit: BTreeMap::new(),
            explode: false,
        }];
        assert!(infer_pagination(&params, &BTreeSet::new()).is_none());
        let fields: BTreeSet<String> = ["next_page_token".to_string()].into_iter().collect();
        assert!(
            matches!(infer_pagination(&params, &fields), Some(PaginationConfig::Cursor { ref param, ref next_path }) if param == "page_token" && next_path == "$.next_page_token")
        );
    }

    #[test]
    fn no_pagination_when_signals_absent() {
        let params = vec![ParamSpec {
            name: "q".into(),
            r#type: "varchar".into(),
            required: false,
            default: None,
            accepts: vec![],
            emit: BTreeMap::new(),
            explode: false,
        }];
        assert!(infer_pagination(&params, &BTreeSet::new()).is_none());
    }

    #[test]
    fn infers_row_cursor_for_starting_after_and_has_more() {
        let params = vec![ParamSpec {
            name: "starting_after".into(),
            r#type: "varchar".into(),
            required: false,
            default: None,
            accepts: vec![],
            emit: BTreeMap::new(),
            explode: false,
        }];
        assert!(infer_pagination(&params, &BTreeSet::new()).is_none());
        let fields: BTreeSet<String> = ["data".into(), "has_more".into()].into_iter().collect();
        assert!(matches!(
            infer_pagination(&params, &fields),
            Some(PaginationConfig::RowCursor { ref param, ref field, ref more_path })
                if param == "starting_after" && field == "id" && more_path.as_deref() == Some("$.has_more")
        ));
    }

    #[test]
    fn header_params_are_skipped_with_diagnostic() {
        let doc = json!({
            "openapi": "3.0.0",
            "paths": { "/x": { "get": {
                "operationId": "getX",
                "parameters": [{ "name": "X-Trace", "in": "header", "schema": { "type": "string" } }],
                "responses": { "200": { "content": { "application/json": { "schema": { "type": "array", "items": { "type": "object", "properties": {} } } } } } }
            }}}
        });
        let s = synth(&doc);
        assert!(table(&s, "get_x").params.is_empty());
        assert!(
            s.diagnostics
                .iter()
                .any(|d| d.code == "OPENAPI_PARAM_LOCATION_SKIPPED")
        );
    }

    #[test]
    fn duplicate_names_are_deduped() {
        let doc = json!({
            "openapi": "3.0.0",
            "paths": {
                "/a": { "get": { "operationId": "list", "responses": {} } },
                "/b": { "get": { "operationId": "list", "responses": {} } }
            }
        });
        let s = synth(&doc);
        let names: BTreeSet<_> = s.tables.iter().map(|t| t.name.clone()).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains("list"));
        assert!(names.contains("list_2"));
    }

    #[test]
    fn include_exclude_select_operations() {
        let doc = json!({
            "openapi": "3.0.0",
            "paths": {
                "/charges": { "get": { "operationId": "charges", "tags": ["Charges"], "responses": {} } },
                "/customers": { "get": { "operationId": "customers", "tags": ["Customers"], "responses": {} } },
                "/test_clocks": { "get": { "operationId": "testClocks", "tags": ["Test"], "responses": {} } }
            }
        });
        let only_charges = SynthOptions {
            include: Selector {
                tags: vec!["Charges".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let s = synthesize(&doc, &only_charges).unwrap();
        assert_eq!(
            s.tables.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["charges"]
        );

        let no_test = SynthOptions {
            exclude: Selector {
                paths: vec!["/test_*".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let s = synthesize(&doc, &no_test).unwrap();
        assert!(s.tables.iter().all(|t| t.name != "test_clocks"));
        assert_eq!(s.tables.len(), 2);
    }

    #[test]
    fn resolves_server_variables() {
        let doc = json!({
            "openapi": "3.0.0",
            "servers": [{ "url": "https://{host}/v1", "variables": { "host": { "default": "api.example.com" } } }],
            "paths": {}
        });
        assert_eq!(
            synth(&doc).base_url.as_deref(),
            Some("https://api.example.com/v1")
        );
    }

    #[test]
    fn snake_case_conversion() {
        assert_eq!(to_snake("listPullRequests"), "list_pull_requests");
        assert_eq!(to_snake("GetChargesCharge"), "get_charges_charge");
        assert_eq!(to_snake("get-by-id"), "get_by_id");
        assert_eq!(to_snake("HTTPServer"), "httpserver");
        assert_eq!(to_snake("v1.Resource"), "v1_resource");
    }

    #[test]
    fn glob_matching() {
        assert!(glob_match("/v1/test_*", "/v1/test_clocks"));
        assert!(!glob_match("/v1/test_*", "/v1/charges"));
        assert!(glob_match("*/pulls", "/repos/x/pulls"));
        assert!(glob_match("/repos/*/pulls", "/repos/octocat/pulls"));
        assert!(glob_match("/exact", "/exact"));
        assert!(!glob_match("/exact", "/exacto"));
    }

    /// Synthesize the real Stripe spec. Point `PAWRLY_STRIPE_SPEC` at a local
    /// `openapi.spec3.yaml`; run with `--ignored`.
    #[test]
    #[ignore = "reads a local Stripe spec via PAWRLY_STRIPE_SPEC"]
    fn stripe_spec_synthesizes() {
        let path = std::env::var("PAWRLY_STRIPE_SPEC").expect("set PAWRLY_STRIPE_SPEC");
        let bytes = std::fs::read(path).expect("read spec");
        let doc: Value = serde_yaml::from_slice(&bytes).expect("parse");
        let s = synthesize(&doc, &opts()).expect("synthesize");

        assert!(s.tables.len() > 150, "only {} tables", s.tables.len());
        for t in &s.tables {
            assert!(!t.response.schema.is_empty(), "{} has no columns", t.name);
            assert!(
                t.endpoint.starts_with('/'),
                "{} endpoint {}",
                t.name,
                t.endpoint
            );
        }

        let charges = table(&s, "get_charges");
        assert_eq!(charges.response.path, "$.data");
        assert!(charges.params.iter().any(|p| p.name == "limit"));
        assert!(
            matches!(charges.pagination, Some(PaginationConfig::RowCursor { ref param, .. }) if param == "starting_after"),
            "get_charges pagination: {:?}",
            charges.pagination
        );

        eprintln!(
            "stripe: {} tables, {} diagnostics",
            s.tables.len(),
            s.diagnostics.len()
        );
    }
}
