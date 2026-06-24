//! HTTP table-valued function executor.
//!
//! A function reuses the table fetch pipeline (see [`crate::fetch`]); the only
//! new work is **spec assembly** — turning a [`FunctionDef`]'s `body` + `args` +
//! `returns` into the [`HttpTableSpec`] a declared table produces.

use std::collections::BTreeMap;
use std::sync::Arc;

use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;
use pawrly_core::{ConfigError, FunctionColumn, FunctionDef};
use serde_json::{Map, Value};

use crate::register::HttpBuildError;
use crate::source::{HttpSource, HttpTableSpec, schema_for};

/// Executes one HTTP table-valued function: binds the call args into the fetch
/// pipeline's params and returns the result batch.
pub struct HttpFunctionExecutor {
    /// Parent source handle (attached) or a freshly built standalone source.
    pub source: Arc<HttpSource>,
    /// Assembled from the function's `body` + `args` + `returns`.
    pub spec: Arc<HttpTableSpec>,
    pub schema: SchemaRef,
    pub max_pages: Option<u32>,
}

impl HttpFunctionExecutor {
    /// Build an executor over a (already-resolved) source handle and a function
    /// definition, assembling the table spec from the function body.
    pub fn new(
        source: Arc<HttpSource>,
        def: &FunctionDef,
        max_pages: Option<u32>,
    ) -> Result<Self, HttpBuildError> {
        let spec = function_spec(def)?;
        let schema = schema_for(&spec);
        Ok(Self {
            source,
            spec: Arc::new(spec),
            schema,
            max_pages,
        })
    }

    /// Run the function for one fully-bound argument set.
    ///
    /// An arg that appears as a `{placeholder}` in `response.path` (e.g.
    /// `http.get`'s `path`) is substituted into the response path per call and
    /// **not** sent to the API.
    pub async fn invoke(
        &self,
        params: &BTreeMap<String, String>,
        limit: Option<usize>,
    ) -> datafusion::common::Result<RecordBatch> {
        let (spec, request_params) = if self.spec.response.path.contains('{') {
            let mut spec = (*self.spec).clone();
            let mut request_params = params.clone();
            for (k, v) in params {
                let needle = format!("{{{k}}}");
                if spec.response.path.contains(&needle) {
                    spec.response.path = spec.response.path.replace(&needle, v);
                    request_params.remove(k);
                }
            }
            (Arc::new(spec), request_params)
        } else {
            (self.spec.clone(), params.clone())
        };

        crate::fetch::fetch_batch(
            self.source.clone(),
            spec,
            &request_params,
            limit,
            self.max_pages,
        )
        .await
    }
}

/// Map a function's `returns` column onto a `response.schema` `ResponseColumn`
/// JSON object. `source: arg` becomes the existing `source: param` echo (the
/// param's bound value is injected as a constant column).
fn response_column_json(c: &FunctionColumn) -> Value {
    let mut o = Map::new();
    o.insert("name".to_string(), Value::String(c.name.clone()));
    o.insert("type".to_string(), Value::String(c.r#type.clone()));
    match c.source.as_deref() {
        Some("arg") => {
            o.insert("source".to_string(), Value::String("param".to_string()));
        }
        Some(other) => {
            o.insert("source".to_string(), Value::String(other.to_string()));
        }
        None => {}
    }
    Value::Object(o)
}

/// Assemble an [`HttpTableSpec`] from a function definition's `body` + `args` +
/// `returns`. The body carries the request shape (`endpoint`, `method`,
/// `headers`, `body`, `pagination`); `args` become `params` (in declared order,
/// no `accepts`/`emit`/`explode` — those are filter-pushdown affordances that do
/// not apply to explicit call args); `returns` become the response schema.
pub fn function_spec(def: &FunctionDef) -> Result<HttpTableSpec, HttpBuildError> {
    let mut map = match def.body.clone() {
        Value::Object(m) => m,
        Value::Null => Map::new(),
        _ => {
            return Err(invalid(def, "http function `body` must be a mapping"));
        }
    };

    map.insert("name".to_string(), Value::String(def.name.clone()));
    if let Some(desc) = &def.description {
        map.entry("description")
            .or_insert_with(|| Value::String(desc.clone()));
    }

    // args -> params (1:1, declared order).
    let params: Vec<Value> = def
        .args
        .iter()
        .map(|a| {
            let mut o = Map::new();
            o.insert("name".to_string(), Value::String(a.name.clone()));
            o.insert("type".to_string(), Value::String(a.r#type.clone()));
            if a.required {
                o.insert("required".to_string(), Value::Bool(true));
            }
            if let Some(d) = &a.default {
                o.insert("default".to_string(), Value::String(d.clone()));
            }
            Value::Object(o)
        })
        .collect();
    map.insert("params".to_string(), Value::Array(params));

    // returns -> response.schema, preserving any `path`/`error`/`reshape` the
    // body already set on `response`.
    let mut response = match map.remove("response") {
        Some(Value::Object(r)) => r,
        Some(Value::Null) | None => Map::new(),
        Some(_) => return Err(invalid(def, "http function `response` must be a mapping")),
    };
    response
        .entry("path")
        .or_insert_with(|| Value::String("$".to_string()));
    response.insert(
        "schema".to_string(),
        Value::Array(def.returns.iter().map(response_column_json).collect()),
    );
    map.insert("response".to_string(), Value::Object(response));

    serde_json::from_value(Value::Object(map))
        .map_err(|e| invalid(def, &format!("invalid http function body: {e}")))
}

fn invalid(def: &FunctionDef, msg: &str) -> HttpBuildError {
    HttpBuildError::Config(ConfigError::FunctionInvalid {
        namespace: def.namespace.clone(),
        name: def.name.clone(),
        msg: msg.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pawrly_core::{FunctionArg, FunctionKind};

    fn arg(name: &str, ty: &str, required: bool, default: Option<&str>) -> FunctionArg {
        FunctionArg {
            name: name.into(),
            r#type: ty.into(),
            required,
            default: default.map(str::to_string),
            description: None,
            tool_arg: None,
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
            namespace: "gh".into(),
            name: "search_issues".into(),
            kind: FunctionKind::Http,
            description: None,
            wiki: None,
            examples: vec![],
            args: vec![
                arg("q", "varchar", true, None),
                arg("limit", "int", false, Some("50")),
            ],
            returns: vec![
                col("number", "bigint", None),
                col("login", "varchar", Some("$.user.login")),
                col("q", "varchar", Some("arg")),
            ],
            connection: Value::Null,
            body: serde_json::json!({
                "endpoint": "/search/issues",
                "response": { "path": "$.items" },
                "pagination": { "type": "page", "param": "page" }
            }),
            source: Some("gh".into()),
            builtin: false,
            cache: Default::default(),
            safety: None,
        }
    }

    #[test]
    fn assembles_endpoint_params_and_response() {
        let spec = function_spec(&def()).expect("spec");
        assert_eq!(spec.name, "search_issues");
        assert_eq!(spec.endpoint, "/search/issues");
        // args -> params, in declared order.
        assert_eq!(spec.params.len(), 2);
        assert_eq!(spec.params[0].name, "q");
        assert!(spec.params[0].required);
        assert_eq!(spec.params[1].name, "limit");
        assert_eq!(spec.params[1].default.as_deref(), Some("50"));
        // returns -> response.schema; path preserved from the body.
        assert_eq!(spec.response.path, "$.items");
        assert_eq!(spec.response.schema.len(), 3);
        // `source: arg` mapped to the `param` echo.
        let q = spec
            .response
            .schema
            .iter()
            .find(|c| c.name == "q")
            .expect("q col");
        assert_eq!(q.source.as_deref(), Some("param"));
        // JSONPath source passed through.
        let login = spec
            .response
            .schema
            .iter()
            .find(|c| c.name == "login")
            .expect("login col");
        assert_eq!(login.source.as_deref(), Some("$.user.login"));
    }

    #[test]
    fn default_response_path_when_body_omits_it() {
        let mut d = def();
        d.body = serde_json::json!({ "endpoint": "/things" });
        let spec = function_spec(&d).expect("spec");
        assert_eq!(spec.response.path, "$");
        assert_eq!(spec.response.schema.len(), 3);
    }

    #[test]
    fn schema_matches_returns() {
        let spec = function_spec(&def()).expect("spec");
        let schema = schema_for(&spec);
        let names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
        assert!(names.contains(&"number"));
        assert!(names.contains(&"login"));
    }
}
