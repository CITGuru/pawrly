//! Shared Arrow → JSON row formatting for transports.
//!
//! Both the MCP tools (`pawrly-mcp`) and the REST surface (`pawrly-server`)
//! render query results the same way, so the conversion lives here once rather
//! than being duplicated per transport.

use arrow_array::{Array, RecordBatch};
use arrow_cast::display::{ArrayFormatter, FormatOptions};
use serde_json::Value;

/// Render up to `max` rows of `batches` as positional JSON cells.
///
/// Returns `(columns, rows, total, truncated)`:
/// - `columns` — field names taken from the first batch's schema.
/// - `rows` — `total` rows, each a `Vec<Value>` aligned to `columns`. A null
///   cell is `Value::Null`; every other cell is a `Value::String` of its
///   display form (Arrow's `ArrayFormatter`).
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
