//! In-engine JSON helper UDFs.
//!
//! `json`-typed source columns arrive as Arrow `Utf8` strings, so querying
//! *into* them needs SQL-level parsing. Two scalar functions cover the common
//! shape — a column holding a JSON array of objects:
//!
//! * `from_json(text) -> List<Utf8>` — parse a JSON array and return each
//!   element as its JSON text, so `unnest(from_json(col))` expands the array
//!   into one row per element. A non-array JSON value yields a single-element
//!   list.
//! * `json_extract_string(text, path) -> Utf8` — pull a field out of a JSON
//!   object by key, or a dotted path (`a.b.c`) for nested objects. String
//!   values come back unquoted; objects/arrays/numbers come back as their JSON
//!   text; a missing key (or JSON null) yields NULL.
//!
//! Both are lenient: NULL input or unparseable JSON yields NULL rather than
//! erroring, so one malformed row never fails the whole scan.
//!
//! `unnest` must sit in a projection, not a correlated lateral join, so explode
//! the array in a subquery and read its elements in the outer query:
//!
//! ```sql
//! WITH elems AS (SELECT unnest(from_json(payload)) AS e FROM t)
//! SELECT json_extract_string(e, 'code')                    AS code,
//!        CAST(json_extract_string(e, 'amount') AS DOUBLE)   AS amount
//! FROM elems;
//! ```

use std::sync::Arc;

use arrow_array::builder::{ListBuilder, StringBuilder};
use arrow_array::{Array, ArrayRef, StringArray};
use arrow_schema::{DataType, Field};
use datafusion::error::{DataFusionError, Result};
use datafusion::logical_expr::{create_udf, ColumnarValue, ScalarUDF, Volatility};
use datafusion::prelude::SessionContext;
use serde_json::Value;

/// Register the JSON SQL functions on `ctx`: our own `from_json` /
/// `json_extract_string`, plus the `datafusion-functions-json` suite
/// (`json_get_*`, `json_length`, `json_contains`, the `->`/`->>`/`?` operators).
/// The two sets share no names.
pub fn register(ctx: &mut SessionContext) -> Result<()> {
    ctx.register_udf(from_json_udf());
    ctx.register_udf(json_extract_string_udf());
    datafusion_functions_json::register_all(ctx)?;
    Ok(())
}

fn from_json_udf() -> ScalarUDF {
    let item = Arc::new(Field::new("item", DataType::Utf8, true));
    create_udf(
        "from_json",
        vec![DataType::Utf8],
        DataType::List(item),
        Volatility::Immutable,
        Arc::new(from_json_impl),
    )
}

fn json_extract_string_udf() -> ScalarUDF {
    create_udf(
        "json_extract_string",
        vec![DataType::Utf8, DataType::Utf8],
        DataType::Utf8,
        Volatility::Immutable,
        Arc::new(json_extract_string_impl),
    )
}

/// `from_json(text) -> List<Utf8>`: explode a JSON array into one Utf8 element
/// per item (each carrying that item's JSON text).
fn from_json_impl(args: &[ColumnarValue]) -> Result<ColumnarValue> {
    let arrays = ColumnarValue::values_to_arrays(args)?;
    let input = utf8_arg(&arrays[0], "from_json")?;

    let mut builder = ListBuilder::new(StringBuilder::new());
    for i in 0..input.len() {
        if input.is_null(i) {
            builder.append_null();
            continue;
        }
        match serde_json::from_str::<Value>(input.value(i)) {
            Ok(Value::Array(elems)) => {
                for e in &elems {
                    builder.values().append_value(value_to_text(e));
                }
                builder.append(true);
            }
            // A non-array (object / scalar) becomes a one-element list, so
            // `unnest(from_json(x))` still yields a usable row.
            Ok(other) => {
                builder.values().append_value(value_to_text(&other));
                builder.append(true);
            }
            // Unparseable JSON → NULL list, never an error.
            Err(_) => builder.append_null(),
        }
    }
    Ok(ColumnarValue::Array(Arc::new(builder.finish())))
}

/// `json_extract_string(text, path) -> Utf8`: read a field out of a JSON object
/// by key or dotted path.
fn json_extract_string_impl(args: &[ColumnarValue]) -> Result<ColumnarValue> {
    let arrays = ColumnarValue::values_to_arrays(args)?;
    let text = utf8_arg(&arrays[0], "json_extract_string")?;
    let paths = utf8_arg(&arrays[1], "json_extract_string")?;

    let mut out = StringBuilder::new();
    for i in 0..text.len() {
        if text.is_null(i) || paths.is_null(i) {
            out.append_null();
            continue;
        }
        let extracted = serde_json::from_str::<Value>(text.value(i))
            .ok()
            .and_then(|v| extract_path(&v, paths.value(i)));
        match extracted {
            None | Some(Value::Null) => out.append_null(),
            Some(Value::String(s)) => out.append_value(s),
            Some(other) => out.append_value(other.to_string()),
        }
    }
    Ok(ColumnarValue::Array(Arc::new(out.finish())))
}

fn utf8_arg<'a>(array: &'a ArrayRef, func: &str) -> Result<&'a StringArray> {
    array.as_any().downcast_ref::<StringArray>().ok_or_else(|| {
        DataFusionError::Execution(format!("{func} expects Utf8 (varchar/json) arguments"))
    })
}

/// Walk a dotted object path (`a.b.c`); a single segment is a plain key lookup.
fn extract_path(root: &Value, path: &str) -> Option<Value> {
    let mut cur = root;
    for seg in path.split('.') {
        cur = cur.get(seg)?;
    }
    Some(cur.clone())
}

/// JSON strings unwrap to their text; everything else keeps its JSON form.
fn value_to_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        _ => v.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::cast::AsArray;
    use arrow_array::ListArray;

    fn utf8_col(values: &[Option<&str>]) -> ColumnarValue {
        ColumnarValue::Array(Arc::new(StringArray::from(values.to_vec())))
    }

    fn run(f: impl Fn(&[ColumnarValue]) -> Result<ColumnarValue>, args: &[ColumnarValue]) -> ArrayRef {
        match f(args).unwrap() {
            ColumnarValue::Array(a) => a,
            ColumnarValue::Scalar(s) => s.to_array().unwrap(),
        }
    }

    #[test]
    fn from_json_explodes_array_of_objects() {
        let input = utf8_col(&[Some(r#"[{"asset_code":"REAL"},{"asset_code":"GOLD"}]"#)]);
        let out = run(from_json_impl, &[input]);
        let list = out.as_any().downcast_ref::<ListArray>().unwrap();
        let elems = list.value(0);
        let elems = elems.as_string::<i32>();
        assert_eq!(elems.len(), 2);
        assert_eq!(elems.value(0), r#"{"asset_code":"REAL"}"#);
        assert_eq!(elems.value(1), r#"{"asset_code":"GOLD"}"#);
    }

    #[test]
    fn from_json_handles_null_and_garbage_and_scalars() {
        let input = utf8_col(&[None, Some("not json"), Some(r#"{"a":1}"#)]);
        let out = run(from_json_impl, &[input]);
        let list = out.as_any().downcast_ref::<ListArray>().unwrap();
        assert!(list.is_null(0)); // NULL input -> NULL list
        assert!(list.is_null(1)); // bad JSON -> NULL list
        assert_eq!(list.value(2).as_string::<i32>().len(), 1); // object -> 1-elem list
    }

    #[test]
    fn json_extract_string_reads_keys_paths_and_types() {
        let text = utf8_col(&[
            Some(r#"{"asset_code":"USDC"}"#),
            Some(r#"{"thresholds":{"low":3}}"#),
            Some(r#"{"flag":true}"#),
            Some(r#"{"other":1}"#),
        ]);
        let path = ColumnarValue::Array(Arc::new(StringArray::from(vec![
            "asset_code",
            "thresholds.low",
            "flag",
            "missing",
        ])));
        let out = run(json_extract_string_impl, &[text, path]);
        let s = out.as_string::<i32>();
        assert_eq!(s.value(0), "USDC"); // string -> unquoted
        assert_eq!(s.value(1), "3"); // nested path -> JSON text
        assert_eq!(s.value(2), "true"); // bool -> JSON text
        assert!(s.is_null(3)); // missing key -> NULL
    }
}
