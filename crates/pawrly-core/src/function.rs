//! Table-valued function types: reusable, named operations callable from SQL as
//! `SELECT * FROM <namespace>.<name>(args...)`.
//!
//! Functions are either **builtin** (shipped with pawrly, constructed by
//! [`builtins`]) or **declared** in YAML — either *source-attached* (inheriting
//! a source's connection config) or *standalone*. Both declaration shapes
//! converge into one engine-facing [`FunctionDef`], so everything downstream
//! (registry, UDTF, executors, CLI, MCP) sees a single type.
//!
//! These are pure data types with no engine dependency. `connection` and `body`
//! are opaque `serde_json::Value`s so this crate needs no dependency on
//! `pawrly-sources`; an executor deserializes them with the same code that
//! parses a source's `config`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cache::CachePolicy;
use crate::model::SourceKind;
use crate::safety::SafetyPolicy;

/// Namespaces a user declaration may not claim: the builtin kinds plus the
/// reserved `materialized` schema. `__` is separately banned in identifiers
/// (it is the UDTF name-mangling separator).
pub const RESERVED_FUNCTION_NAMESPACES: [&str; 4] = ["http", "file", "mcp", "materialized"];

/// Execution backend for a function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum FunctionKind {
    /// Templated HTTP request → rows.
    Http,
    /// External MCP `tools/call` → rows.
    Mcp,
    /// Glob → file-metadata rows.
    File,
}

impl FunctionKind {
    /// The function kind a source of `kind` may carry **attached** functions
    /// for: `Http→Http`, `Mcp→Mcp`, `File→File`, anything else `None`.
    #[must_use]
    pub fn for_source(kind: SourceKind) -> Option<Self> {
        match kind {
            SourceKind::Http => Some(Self::Http),
            SourceKind::Mcp => Some(Self::Mcp),
            SourceKind::File => Some(Self::File),
            _ => None,
        }
    }

    /// Lowercase wire name (`"http"`, `"mcp"`, `"file"`).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Mcp => "mcp",
            Self::File => "file",
        }
    }
}

impl std::fmt::Display for FunctionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

fn default_arg_type() -> String {
    "varchar".to_string()
}

/// One declared argument. **List order is the positional call order**: required
/// args precede optional/defaulted ones so a positional call is unambiguous.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FunctionArg {
    pub name: String,

    /// Column-vocabulary type (`varchar`, `int`, `bigint`, `double`, `bool`,
    /// `timestamp`, ...). Defaults to `varchar`. Drives CLI/MCP literal
    /// rendering and documentation; the HTTP pipeline itself is stringly.
    #[serde(rename = "type", default = "default_arg_type")]
    pub r#type: String,

    #[serde(default)]
    pub required: bool,

    /// Value used when the call omits this trailing arg. Mutually exclusive with
    /// `required`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// mcp only: wire name of the tool argument when it differs from `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_arg: Option<String>,
}

/// One output column. `source` is either a JSONPath into each response row
/// (`$.user.login`) or the literal `arg`, which injects the bound call argument
/// of the same name.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FunctionColumn {
    pub name: String,

    #[serde(rename = "type")]
    pub r#type: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Engine-facing, fully-resolved function descriptor — one type for builtin,
/// source-attached, and standalone functions.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FunctionDef {
    pub namespace: String,
    pub name: String,
    pub kind: FunctionKind,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wiki: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<FunctionArg>,

    /// Output columns; the schema is fixed at plan time (the reason `returns` is
    /// explicit rather than inferred — inference would require live calls during
    /// planning). Non-empty.
    pub returns: Vec<FunctionColumn>,

    /// Connection config (`base_url`/`auth`/`rate_limit` for http; `transport` +
    /// `command`/`url` for mcp). For attached functions: a clone of the parent
    /// source's `config`. Opaque here; deserialized by the executor.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub connection: Value,

    /// Kind-specific body (http: endpoint/response/pagination | mcp:
    /// tool/tool_args/rows_path/pagination/limit_binding | file: path/format).
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub body: Value,

    /// Parent source name when attached, so the engine can reuse the live
    /// source handle (shared rate-limiter / MCP session) and tear the function
    /// down with its source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,

    #[serde(default)]
    pub builtin: bool,

    /// Reserved; cache is inert in v1.
    #[serde(default)]
    pub cache: CachePolicy,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety: Option<SafetyPolicy>,
}

impl FunctionDef {
    /// Fully-qualified `namespace.name`.
    #[must_use]
    pub fn qualified_name(&self) -> String {
        format!("{}.{}", self.namespace, self.name)
    }

    /// A human-readable signature, e.g. `github.search_issues(q varchar, limit int = 50)`.
    #[must_use]
    pub fn signature(&self) -> String {
        let args: Vec<String> = self
            .args
            .iter()
            .map(|a| {
                let mut s = format!("{} {}", a.name, a.r#type);
                if let Some(d) = &a.default {
                    s.push_str(&format!(" = {d}"));
                }
                s
            })
            .collect();
        format!("{}({})", self.qualified_name(), args.join(", "))
    }

    /// Summary form for `list_functions`.
    #[must_use]
    pub fn info(&self) -> FunctionInfo {
        FunctionInfo {
            namespace: self.namespace.clone(),
            name: self.name.clone(),
            kind: self.kind,
            builtin: self.builtin,
            signature: self.signature(),
            description: self.description.clone(),
        }
    }

    /// Full form for `describe_function`.
    #[must_use]
    pub fn describe(&self) -> FunctionDescription {
        FunctionDescription {
            namespace: self.namespace.clone(),
            name: self.name.clone(),
            kind: self.kind,
            builtin: self.builtin,
            signature: self.signature(),
            description: self.description.clone(),
            wiki: self.wiki.clone(),
            examples: self.examples.clone(),
            args: self.args.clone(),
            returns: self.returns.clone(),
        }
    }
}

/// Summary descriptor returned by `list_functions`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FunctionInfo {
    pub namespace: String,
    pub name: String,
    pub kind: FunctionKind,
    pub builtin: bool,
    pub signature: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Full descriptor returned by `describe_function`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FunctionDescription {
    pub namespace: String,
    pub name: String,
    pub kind: FunctionKind,
    pub builtin: bool,
    pub signature: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wiki: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
    pub args: Vec<FunctionArg>,
    pub returns: Vec<FunctionColumn>,
}

/// One positional argument for [`render_call_sql`], already ordered into the
/// declaration's call order.
#[derive(Debug, Clone)]
pub struct CallArg {
    /// The literal value as a string.
    pub value: String,
    /// Render as a single-quoted SQL string literal (`true`) or a bare
    /// numeric/boolean literal (`false`).
    pub quoted: bool,
}

impl CallArg {
    /// Build a call argument, deciding quoting from the declared `type_str`
    /// (numeric/boolean → bare literal, everything else → quoted string). A
    /// numeric/boolean arg whose value is *not* a genuine bare literal (e.g. an
    /// `int` arg supplied as `1 OR 1=1`) falls back to a quoted string literal so
    /// it can never break out of the argument position in the string-composed
    /// CLI/MCP `call` SQL.
    #[must_use]
    pub fn new(value: impl Into<String>, type_str: &str) -> Self {
        let value = value.into();
        let quoted = type_is_string(type_str) || !is_safe_bare_literal(&value);
        Self { value, quoted }
    }
}

/// Whether `value` is a self-contained numeric or boolean SQL literal — safe to
/// emit bare in composed SQL. Anything else (including injection attempts like
/// `1 OR 1=1`) returns `false` and must be quoted.
fn is_safe_bare_literal(value: &str) -> bool {
    if value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("false") {
        return true;
    }
    // `f64::from_str` also accepts `inf`/`nan`/`infinity`; reject any stray
    // letters (keeping the exponent marker `e`/`E`) so only sign/digit/point/
    // exponent forms pass through as bare numerics.
    if value
        .chars()
        .any(|c| c.is_ascii_alphabetic() && !matches!(c, 'e' | 'E'))
    {
        return false;
    }
    value.parse::<i64>().is_ok() || value.parse::<f64>().is_ok()
}

/// Whether a declared column/arg type renders as a quoted SQL string literal.
/// Numeric and boolean types render bare; everything else is quoted.
#[must_use]
pub fn type_is_string(type_str: &str) -> bool {
    !matches!(
        type_str.trim().to_ascii_lowercase().as_str(),
        "int"
            | "integer"
            | "int32"
            | "int64"
            | "bigint"
            | "long"
            | "smallint"
            | "double"
            | "float"
            | "float32"
            | "float64"
            | "real"
            | "decimal"
            | "numeric"
            | "bool"
            | "boolean"
    )
}

/// Render a `SELECT * FROM namespace.name(args...)` call. The single
/// quoting/escaping implementation shared by the CLI `function call` command and
/// the MCP `call_function` tool, so both compose identical SQL. `namespace` and
/// `name` are validated identifiers, so they need no escaping; string args are
/// single-quoted with embedded `'` doubled.
#[must_use]
pub fn render_call_sql(namespace: &str, name: &str, args: &[CallArg]) -> String {
    let rendered: Vec<String> = args
        .iter()
        .map(|a| {
            if a.quoted {
                format!("'{}'", a.value.replace('\'', "''"))
            } else {
                a.value.clone()
            }
        })
        .collect();
    format!("SELECT * FROM {namespace}.{name}({})", rendered.join(", "))
}

fn col(name: &str, r#type: &str) -> FunctionColumn {
    FunctionColumn {
        name: name.to_string(),
        r#type: r#type.to_string(),
        source: None,
        description: None,
    }
}

fn required_arg(name: &str, r#type: &str, description: &str) -> FunctionArg {
    FunctionArg {
        name: name.to_string(),
        r#type: r#type.to_string(),
        required: true,
        default: None,
        description: Some(description.to_string()),
        tool_arg: None,
    }
}

/// The builtin functions, present without any YAML. Listing, describing, and
/// calling them goes through exactly the same paths as declared functions
/// (`builtin: true`); their namespaces are reserved (see
/// [`RESERVED_FUNCTION_NAMESPACES`]).
#[must_use]
pub fn builtins() -> Vec<FunctionDef> {
    vec![http_get(), file_glob(), file_grep()]
}

/// `http.get(url, path)` → generic GET. `path` is a JSONPath into the response
/// (templated into `response.path` per call, not sent to the API); each matched
/// element is returned as a JSON string in the single `body` column.
fn http_get() -> FunctionDef {
    FunctionDef {
        namespace: "http".to_string(),
        name: "get".to_string(),
        kind: FunctionKind::Http,
        description: Some(
            "Generic HTTP GET; `path` is a JSONPath into the response body. Each matched \
             element is returned as JSON in the `body` column."
                .to_string(),
        ),
        wiki: None,
        examples: vec![
            "SELECT body FROM http.get('https://api.example.com/items', '$.items')".to_string(),
        ],
        args: vec![
            required_arg("url", "varchar", "Absolute URL to GET."),
            FunctionArg {
                name: "path".to_string(),
                r#type: "varchar".to_string(),
                required: false,
                default: Some("$".to_string()),
                description: Some("JSONPath into the response body (default `$`).".to_string()),
                tool_arg: None,
            },
        ],
        returns: vec![FunctionColumn {
            name: "body".to_string(),
            r#type: "varchar".to_string(),
            source: Some("$".to_string()),
            description: Some("The matched element, as a JSON string.".to_string()),
        }],
        connection: Value::Null,
        body: serde_json::json!({ "endpoint": "{url}", "response": { "path": "{path}" } }),
        source: None,
        builtin: true,
        cache: CachePolicy::default(),
        safety: None,
    }
}

/// `file.glob(pattern)` → one row per matched file. Fully specified: the schema
/// is deterministic (`path`, `file_name`, `size_bytes`, `modified`).
fn file_glob() -> FunctionDef {
    FunctionDef {
        namespace: "file".to_string(),
        name: "glob".to_string(),
        kind: FunctionKind::File,
        description: Some("List files matching a glob pattern, one row per file.".to_string()),
        wiki: None,
        examples: vec![
            "SELECT file_name, size_bytes FROM file.glob('./data/*.parquet') ORDER BY 1"
                .to_string(),
        ],
        args: vec![required_arg(
            "pattern",
            "varchar",
            "Glob pattern, resolved against the workspace dir when relative.",
        )],
        returns: vec![
            col("path", "varchar"),
            col("file_name", "varchar"),
            col("size_bytes", "bigint"),
            col("modified", "timestamp"),
        ],
        connection: Value::Null,
        body: serde_json::json!({ "path": "{pattern}" }),
        source: None,
        builtin: true,
        cache: CachePolicy::default(),
        safety: None,
    }
}

/// `file.grep(pattern, glob)` → one row per line matching the regex `pattern`
/// across the files matched by `glob`. Schema: `path`, `line_number`, `line`.
fn file_grep() -> FunctionDef {
    FunctionDef {
        namespace: "file".to_string(),
        name: "grep".to_string(),
        kind: FunctionKind::File,
        description: Some(
            "Search file contents with a regex, one row per matching line.".to_string(),
        ),
        wiki: None,
        examples: vec![
            "SELECT path, line_number, line FROM file.grep('ERROR', './logs/*.log')".to_string(),
        ],
        args: vec![
            required_arg(
                "pattern",
                "varchar",
                "Regular expression matched against each line.",
            ),
            required_arg(
                "glob",
                "varchar",
                "Glob of files to search, resolved against the workspace dir when relative.",
            ),
        ],
        returns: vec![
            col("path", "varchar"),
            col("line_number", "bigint"),
            col("line", "varchar"),
        ],
        connection: Value::Null,
        body: serde_json::json!({ "path": "{glob}", "grep": "{pattern}" }),
        source: None,
        builtin: true,
        cache: CachePolicy::default(),
        safety: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_source_maps_only_http_mcp_file() {
        assert_eq!(
            FunctionKind::for_source(SourceKind::Http),
            Some(FunctionKind::Http)
        );
        assert_eq!(
            FunctionKind::for_source(SourceKind::Mcp),
            Some(FunctionKind::Mcp)
        );
        assert_eq!(
            FunctionKind::for_source(SourceKind::File),
            Some(FunctionKind::File)
        );
        assert_eq!(FunctionKind::for_source(SourceKind::Postgres), None);
        assert_eq!(FunctionKind::for_source(SourceKind::Duckdb), None);
    }

    #[test]
    fn render_call_sql_quotes_strings_and_escapes() {
        let args = vec![
            CallArg::new("is:open it's", "varchar"),
            CallArg::new("50", "int"),
        ];
        assert_eq!(
            render_call_sql("github", "search_issues", &args),
            "SELECT * FROM github.search_issues('is:open it''s', 50)"
        );
    }

    #[test]
    fn render_call_sql_quotes_injection_in_numeric_args() {
        // A numeric-typed arg whose value isn't a real number must not be
        // emitted bare — it falls back to a quoted (escaped) string literal so
        // it cannot alter the composed SQL.
        let args = vec![CallArg::new("1 OR 1=1", "int")];
        assert_eq!(
            render_call_sql("github", "search_issues", &args),
            "SELECT * FROM github.search_issues('1 OR 1=1')"
        );

        // Genuine numerics and booleans still render bare; negatives, floats and
        // exponents are preserved.
        for (value, ty, want) in [
            ("50", "int", "50"),
            ("-7", "bigint", "-7"),
            ("3.14", "double", "3.14"),
            ("1.5e3", "double", "1.5e3"),
            ("true", "bool", "true"),
            ("FALSE", "boolean", "FALSE"),
        ] {
            let arg = CallArg::new(value, ty);
            assert!(!arg.quoted, "{value} ({ty}) should render bare");
            assert_eq!(arg.value, want);
        }

        // Non-literal values for numeric/bool types are forced to quoted.
        for (value, ty) in [
            ("0); DROP TABLE x; --", "int"),
            ("inf", "double"),
            ("nan", "double"),
            ("1; SELECT", "bigint"),
            ("true OR 1=1", "bool"),
        ] {
            assert!(
                CallArg::new(value, ty).quoted,
                "{value} ({ty}) must be quoted"
            );
        }
    }

    #[test]
    fn render_call_sql_no_args() {
        assert_eq!(
            render_call_sql("file", "glob", &[]),
            "SELECT * FROM file.glob()"
        );
    }

    #[test]
    fn type_is_string_classifies_numeric_and_bool_as_bare() {
        for t in ["int", "BIGINT", "double", "Bool", "boolean", "float64"] {
            assert!(!type_is_string(t), "{t} should render bare");
        }
        for t in ["varchar", "timestamp", "text", "date"] {
            assert!(type_is_string(t), "{t} should render quoted");
        }
    }

    #[test]
    fn builtins_are_present_and_reserved() {
        let b = builtins();
        let names: Vec<String> = b.iter().map(FunctionDef::qualified_name).collect();
        assert!(names.contains(&"file.glob".to_string()));
        for f in &b {
            assert!(f.builtin);
            assert!(RESERVED_FUNCTION_NAMESPACES.contains(&f.namespace.as_str()));
            assert!(!f.returns.is_empty());
        }
    }

    #[test]
    fn signature_renders_args_with_defaults() {
        let g = builtins()
            .into_iter()
            .find(|f| f.name == "glob")
            .expect("glob builtin");
        assert_eq!(g.signature(), "file.glob(pattern varchar)");
    }
}
