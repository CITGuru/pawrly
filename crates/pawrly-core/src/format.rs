//! Shared Arrow → JSON row formatting for transports.
//!
//! Both the MCP tools (`pawrly-mcp`) and the REST surface (`pawrly-server`)
//! render query results the same way, so the conversion lives here once rather
//! than being duplicated per transport.

use arrow_array::cast::AsArray;
use arrow_array::types::{
    Float32Type, Float64Type, Int8Type, Int16Type, Int32Type, Int64Type, UInt8Type, UInt16Type,
    UInt32Type, UInt64Type,
};
use arrow_array::{Array, RecordBatch};
use arrow_cast::display::{ArrayFormatter, FormatOptions};
use arrow_schema::DataType;
use serde_json::Value;

/// Convert one cell — `array[row]`, with `fmt` the [`ArrayFormatter`] for
/// `array` — to typed JSON. Integers and floats become JSON numbers, booleans
/// JSON bools, nulls `null`; every other Arrow type (temporal, decimal, string,
/// binary, nested) falls back to its display string, as does a non-finite float
/// (NaN/±Inf) that JSON cannot represent.
#[must_use]
pub fn cell_to_json(array: &dyn Array, row: usize, fmt: &ArrayFormatter<'_>) -> Value {
    if array.is_null(row) {
        return Value::Null;
    }
    match array.data_type() {
        DataType::Boolean => Value::Bool(array.as_boolean().value(row)),
        DataType::Int8 => Value::from(array.as_primitive::<Int8Type>().value(row)),
        DataType::Int16 => Value::from(array.as_primitive::<Int16Type>().value(row)),
        DataType::Int32 => Value::from(array.as_primitive::<Int32Type>().value(row)),
        DataType::Int64 => Value::from(array.as_primitive::<Int64Type>().value(row)),
        DataType::UInt8 => Value::from(array.as_primitive::<UInt8Type>().value(row)),
        DataType::UInt16 => Value::from(array.as_primitive::<UInt16Type>().value(row)),
        DataType::UInt32 => Value::from(array.as_primitive::<UInt32Type>().value(row)),
        DataType::UInt64 => Value::from(array.as_primitive::<UInt64Type>().value(row)),
        DataType::Float32 => f64_cell(
            f64::from(array.as_primitive::<Float32Type>().value(row)),
            fmt,
            row,
        ),
        DataType::Float64 => f64_cell(array.as_primitive::<Float64Type>().value(row), fmt, row),
        _ => Value::String(fmt.value(row).to_string()),
    }
}

/// JSON number for a finite float; display string otherwise (JSON has no
/// NaN/Inf).
fn f64_cell(x: f64, fmt: &ArrayFormatter<'_>, row: usize) -> Value {
    serde_json::Number::from_f64(x)
        .map_or_else(|| Value::String(fmt.value(row).to_string()), Value::Number)
}

/// Render up to `max` rows of `batches` as positional JSON cells.
///
/// Returns `(columns, rows, total, truncated)`:
/// - `columns` — field names taken from the first batch's schema.
/// - `rows` — `total` rows, each a `Vec<Value>` aligned to `columns`, typed via
///   [`cell_to_json`] (numbers/bools/null typed; other types as strings).
/// - `total` — number of rows emitted (`≤ max`).
/// - `truncated` — `true` when more rows existed beyond `max`.
///
/// A batch whose columns can't be formatted is skipped rather than failing the
/// whole result.
#[must_use]
pub fn format_batches(
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
                .map(|(i, f)| cell_to_json(batch.column(i).as_ref(), r, f))
                .collect();
            rows.push(row);
            total += 1;
        }
    }
    (columns, rows, total, truncated)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]

    use std::sync::Arc;

    use arrow_array::{BooleanArray, Float64Array, Int64Array, StringArray};
    use arrow_schema::{Field, Schema};

    use super::*;

    #[test]
    fn typed_cells_and_nulls() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("i", DataType::Int64, true),
            Field::new("f", DataType::Float64, true),
            Field::new("b", DataType::Boolean, true),
            Field::new("s", DataType::Utf8, true),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int64Array::from(vec![Some(42), None])),
                Arc::new(Float64Array::from(vec![Some(1.5), Some(2.0)])),
                Arc::new(BooleanArray::from(vec![Some(true), Some(false)])),
                Arc::new(StringArray::from(vec![Some("x"), Some("y")])),
            ],
        )
        .unwrap();

        let (cols, rows, total, truncated) = format_batches(&[batch], 10);
        assert_eq!(cols, ["i", "f", "b", "s"]);
        assert_eq!(total, 2);
        assert!(!truncated);
        assert_eq!(rows[0][0], Value::from(42));
        assert!(rows[0][0].is_number());
        assert_eq!(rows[0][1], Value::from(1.5));
        assert_eq!(rows[0][2], Value::Bool(true));
        assert_eq!(rows[0][3], Value::from("x"));
        assert_eq!(rows[1][0], Value::Null);
        assert_eq!(rows[1][2], Value::Bool(false));
    }

    #[test]
    fn truncation_flag() {
        let schema = Arc::new(Schema::new(vec![Field::new("i", DataType::Int64, false)]));
        let batch =
            RecordBatch::try_new(schema, vec![Arc::new(Int64Array::from(vec![1, 2, 3]))]).unwrap();
        let (_cols, rows, total, truncated) = format_batches(&[batch], 2);
        assert_eq!(total, 2);
        assert_eq!(rows.len(), 2);
        assert!(truncated);
    }
}
