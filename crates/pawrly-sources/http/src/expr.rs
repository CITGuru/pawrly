//! Computed response-column expressions.
//!
//! A response column may declare an `expr` tree that is evaluated against each
//! JSON row into a single value, covering the cases a plain JSONPath `source`
//! can't: first-non-null coalescing, array/tag projections, and small
//! timestamp / base64 / string transforms.

use std::collections::BTreeMap;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A computed column value, evaluated against one response row plus the bound
/// request params. Selected by a `kind` tag in YAML.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ColumnExpr {
    /// Walk object keys `a.b.c` from the row.
    Path { path: Vec<String> },
    /// The whole row element.
    CurrentRow,
    /// Always null.
    Null,
    /// A bound request parameter, by name.
    FromFilter { filter: String },
    /// First non-null sub-expression.
    Coalesce { exprs: Vec<ColumnExpr> },
    /// Convert an epoch number (`unit`: `seconds` | `millis`) to an RFC 3339
    /// UTC timestamp string. A string input passes through unchanged.
    ToTimestamp { unit: String, expr: Box<ColumnExpr> },
    /// Join the array at `path` into a comma-separated string (scalars as text,
    /// objects/arrays as compact JSON).
    Join { path: Vec<String> },
    /// For each element of the array at `path`, take `item_path`, joined by `,`.
    MapJoin {
        path: Vec<String>,
        item_path: Vec<String>,
    },
    /// `item_path` of the first element of the array at `path`.
    FirstOf {
        path: Vec<String>,
        item_path: Vec<String>,
    },
    /// In the array of objects at `path`, the `value_field` of the element whose
    /// `key_field` equals `key`. `key_field`/`value_field` default to
    /// `key`/`value` (the common `[{key, value}]` tag shape).
    Lookup {
        path: Vec<String>,
        key: String,
        #[serde(default = "default_key_field")]
        key_field: String,
        #[serde(default = "default_value_field")]
        value_field: String,
    },
    /// Like `lookup` but joins every matching element's `value_field`.
    LookupJoin {
        path: Vec<String>,
        key: String,
        #[serde(default = "default_key_field")]
        key_field: String,
        #[serde(default = "default_value_field")]
        value_field: String,
    },
    /// In the object at `path`, select the entry keyed by the bound param `by`,
    /// then take `item_path`.
    Pick {
        path: Vec<String>,
        by: String,
        item_path: Vec<String>,
    },
    /// A constant value.
    Literal { value: Value },
    /// `then_value` when `check` evaluates to a non-null value, else null.
    IfPresent {
        check: Box<ColumnExpr>,
        then_value: Value,
    },
    /// Base64-decode the string produced by `expr` into UTF-8 text.
    #[serde(rename = "from_base64")]
    FromBase64 { expr: Box<ColumnExpr> },
    /// Replace every `from` with `to` in the string produced by `expr`.
    Replace {
        expr: Box<ColumnExpr>,
        from: String,
        to: String,
    },
}

impl ColumnExpr {
    /// Evaluate against one row. Returns `None` (rendered as SQL `NULL`) when a
    /// path is missing, a shape doesn't match, or a transform fails.
    pub fn eval(&self, row: &Value, params: &BTreeMap<String, String>) -> Option<Value> {
        match self {
            Self::Path { path } => walk(row, path).cloned(),
            Self::CurrentRow => Some(row.clone()),
            Self::Null => None,
            Self::FromFilter { filter } => params.get(filter).cloned().map(Value::String),
            Self::Coalesce { exprs } => exprs
                .iter()
                .filter_map(|e| e.eval(row, params))
                .find(|v| !v.is_null()),
            Self::ToTimestamp { unit, expr } => {
                to_rfc3339(&expr.eval(row, params)?, unit).map(Value::String)
            }
            Self::Join { path } => {
                let arr = walk(row, path)?.as_array()?;
                Some(Value::String(join(arr.iter())))
            }
            Self::MapJoin { path, item_path } => {
                let arr = walk(row, path)?.as_array()?;
                Some(Value::String(join(
                    arr.iter().filter_map(|el| walk(el, item_path)),
                )))
            }
            Self::FirstOf { path, item_path } => {
                walk(walk(row, path)?.as_array()?.first()?, item_path).cloned()
            }
            Self::Lookup {
                path,
                key,
                key_field,
                value_field,
            } => {
                let arr = walk(row, path)?.as_array()?;
                arr.iter()
                    .find(|el| el.get(key_field).and_then(Value::as_str) == Some(key))
                    .and_then(|el| el.get(value_field).cloned())
            }
            Self::LookupJoin {
                path,
                key,
                key_field,
                value_field,
            } => {
                let arr = walk(row, path)?.as_array()?;
                Some(Value::String(join(
                    arr.iter()
                        .filter(|el| el.get(key_field).and_then(Value::as_str) == Some(key))
                        .filter_map(|el| el.get(value_field)),
                )))
            }
            Self::Pick {
                path,
                by,
                item_path,
            } => {
                let obj = walk(row, path)?;
                let key = params.get(by)?;
                walk(obj.get(key)?, item_path).cloned()
            }
            Self::Literal { value } => Some(value.clone()),
            Self::IfPresent { check, then_value } => check
                .eval(row, params)
                .filter(|v| !v.is_null())
                .map(|_| then_value.clone()),
            Self::FromBase64 { expr } => {
                let v = expr.eval(row, params)?;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(v.as_str()?.trim())
                    .ok()?;
                String::from_utf8(bytes).ok().map(Value::String)
            }
            Self::Replace { expr, from, to } => {
                let v = expr.eval(row, params)?;
                Some(Value::String(v.as_str()?.replace(from, to)))
            }
        }
    }
}

fn default_key_field() -> String {
    "key".into()
}

fn default_value_field() -> String {
    "value".into()
}

/// Walk object keys `a.b.c` from a JSON value.
fn walk<'a>(mut current: &'a Value, path: &[String]) -> Option<&'a Value> {
    for seg in path {
        current = current.get(seg)?;
    }
    Some(current)
}

/// Comma-join JSON values as text (strings unquoted, objects/arrays as JSON).
fn join<'a>(values: impl Iterator<Item = &'a Value>) -> String {
    values.map(value_to_text).collect::<Vec<_>>().join(",")
}

/// Render a JSON value as text: strings unquoted, null empty, everything else
/// (numbers, bools, objects, arrays) as its compact JSON form.
fn value_to_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Convert an epoch value to an RFC 3339 UTC string. `unit` is `seconds` or
/// `millis`; a string value passes through unchanged.
fn to_rfc3339(v: &Value, unit: &str) -> Option<String> {
    use chrono::TimeZone;
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    let n = v.as_f64()?;
    let millis = if matches!(unit, "millis" | "milliseconds") {
        n as i64
    } else {
        (n * 1000.0) as i64
    };
    chrono::Utc
        .timestamp_millis_opt(millis)
        .single()
        .map(|dt| dt.to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expr(json: serde_json::Value) -> ColumnExpr {
        serde_json::from_value(json).expect("valid expr")
    }
    fn row(json: serde_json::Value) -> Value {
        json
    }
    fn no_params() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    #[test]
    fn coalesce_takes_first_non_null() {
        let e = expr(serde_json::json!({
            "kind": "coalesce",
            "exprs": [
                {"kind": "path", "path": ["attributes", "title"]},
                {"kind": "path", "path": ["title"]}
            ]
        }));
        assert_eq!(
            e.eval(&row(serde_json::json!({"title": "fallback"})), &no_params()),
            Some(Value::String("fallback".into()))
        );
        assert_eq!(
            e.eval(
                &row(serde_json::json!({"attributes": {"title": "primary"}, "title": "fallback"})),
                &no_params()
            ),
            Some(Value::String("primary".into()))
        );
    }

    #[test]
    fn map_join_projects_and_joins() {
        let e = expr(serde_json::json!({
            "kind": "map_join", "path": ["labels"], "item_path": ["name"]
        }));
        let r = row(serde_json::json!({"labels": [{"name": "bug"}, {"name": "p1"}]}));
        assert_eq!(
            e.eval(&r, &no_params()),
            Some(Value::String("bug,p1".into()))
        );
    }

    #[test]
    fn first_of_preserves_type() {
        let e = expr(serde_json::json!({
            "kind": "first_of", "path": ["thresholds"], "item_path": ["target"]
        }));
        let r = row(serde_json::json!({"thresholds": [{"target": 1.5}, {"target": 9.0}]}));
        assert_eq!(e.eval(&r, &no_params()), Some(serde_json::json!(1.5)));
    }

    #[test]
    fn lookup_finds_by_key() {
        let e = expr(serde_json::json!({
            "kind": "lookup", "path": ["payload", "headers"],
            "key": "From", "key_field": "name", "value_field": "value"
        }));
        let r = row(serde_json::json!({"payload": {"headers": [
            {"name": "To", "value": "a@x"}, {"name": "From", "value": "b@y"}
        ]}}));
        assert_eq!(e.eval(&r, &no_params()), Some(Value::String("b@y".into())));
    }

    #[test]
    fn pick_uses_bound_param() {
        let e = expr(serde_json::json!({
            "kind": "pick", "path": ["environments"],
            "by": "environment_key", "item_path": ["on"]
        }));
        let r =
            row(serde_json::json!({"environments": {"prod": {"on": true}, "dev": {"on": false}}}));
        let mut params = no_params();
        params.insert("environment_key".into(), "prod".into());
        assert_eq!(e.eval(&r, &params), Some(Value::Bool(true)));
        // Missing param -> null.
        assert_eq!(e.eval(&r, &no_params()), None);
    }

    #[test]
    fn to_timestamp_seconds_to_rfc3339() {
        let e = expr(serde_json::json!({
            "kind": "to_timestamp", "unit": "seconds",
            "expr": {"kind": "path", "path": ["Timestamp"]}
        }));
        let r = row(serde_json::json!({"Timestamp": 1_700_000_000}));
        assert_eq!(
            e.eval(&r, &no_params()),
            Some(Value::String("2023-11-14T22:13:20+00:00".into()))
        );
    }

    #[test]
    fn from_base64_and_replace() {
        let b64 = expr(serde_json::json!({
            "kind": "from_base64", "expr": {"kind": "path", "path": ["content"]}
        }));
        let r = row(serde_json::json!({"content": "aGVsbG8="}));
        assert_eq!(
            b64.eval(&r, &no_params()),
            Some(Value::String("hello".into()))
        );

        let rep = expr(serde_json::json!({
            "kind": "replace", "expr": {"kind": "path", "path": ["id"]}, "from": "#", "to": "%23"
        }));
        let r2 = row(serde_json::json!({"id": "a#b#c"}));
        assert_eq!(
            rep.eval(&r2, &no_params()),
            Some(Value::String("a%23b%23c".into()))
        );
    }
}
