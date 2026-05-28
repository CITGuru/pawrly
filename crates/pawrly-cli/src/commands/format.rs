//! Output formatters for `pawrly sql`.

use std::io::Write;
use std::sync::Arc;

use arrow_array::{Array, RecordBatch};
use arrow_cast::display::{ArrayFormatter, FormatOptions};
use arrow_schema::Schema;
use clap::ValueEnum;
use comfy_table::{ContentArrangement, Table};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Table,
    Json,
    Ndjson,
    Csv,
}

impl Format {
    pub fn write_batches<W: Write>(
        self,
        writer: &mut W,
        batches: &[RecordBatch],
    ) -> anyhow::Result<()> {
        match self {
            Format::Table => write_table(writer, batches),
            Format::Json => write_json(writer, batches, false),
            Format::Ndjson => write_json(writer, batches, true),
            Format::Csv => write_csv(writer, batches),
        }
    }
}

fn write_table<W: Write>(writer: &mut W, batches: &[RecordBatch]) -> anyhow::Result<()> {
    let Some(schema) = batches.first().map(|b| b.schema()) else {
        writeln!(writer, "(0 rows)")?;
        return Ok(());
    };

    let mut table = Table::new();
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(schema.fields().iter().map(|f| f.name().clone()));
    let mut total = 0usize;
    let opts = FormatOptions::default();
    for batch in batches {
        let formatters: Vec<ArrayFormatter<'_>> = batch
            .columns()
            .iter()
            .map(|c| ArrayFormatter::try_new(c.as_ref(), &opts))
            .collect::<Result<_, _>>()?;
        for row in 0..batch.num_rows() {
            let cells: Vec<String> = formatters
                .iter()
                .map(|f| {
                    let mut s = String::new();
                    let _ = std::fmt::Write::write_fmt(&mut s, format_args!("{}", f.value(row)));
                    s
                })
                .collect();
            table.add_row(cells);
            total += 1;
        }
    }
    writeln!(writer, "{table}")?;
    writeln!(writer, "({total} row{})", if total == 1 { "" } else { "s" })?;
    Ok(())
}

fn write_json<W: Write>(
    writer: &mut W,
    batches: &[RecordBatch],
    ndjson: bool,
) -> anyhow::Result<()> {
    if !ndjson {
        write!(writer, "[")?;
    }
    let mut first = true;
    for batch in batches {
        let rows = batch_to_rows(batch)?;
        for row in rows {
            if ndjson {
                serde_json::to_writer(&mut *writer, &row)?;
                writeln!(writer)?;
            } else {
                if !first {
                    write!(writer, ",")?;
                }
                serde_json::to_writer(&mut *writer, &row)?;
                first = false;
            }
        }
    }
    if !ndjson {
        writeln!(writer, "]")?;
    }
    Ok(())
}

fn write_csv<W: Write>(writer: &mut W, batches: &[RecordBatch]) -> anyhow::Result<()> {
    let Some(first) = batches.first() else {
        return Ok(());
    };
    let schema = first.schema();
    write_csv_row(
        writer,
        &schema
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect::<Vec<_>>(),
    )?;
    let opts = FormatOptions::default();
    for batch in batches {
        let formatters: Vec<ArrayFormatter<'_>> = batch
            .columns()
            .iter()
            .map(|c| ArrayFormatter::try_new(c.as_ref(), &opts))
            .collect::<Result<_, _>>()?;
        for row in 0..batch.num_rows() {
            let cells: Vec<String> = formatters
                .iter()
                .map(|f| format!("{}", f.value(row)))
                .collect();
            write_csv_row(writer, &cells)?;
        }
    }
    Ok(())
}

fn write_csv_row<W: Write>(writer: &mut W, cells: &[String]) -> anyhow::Result<()> {
    let mut first = true;
    for c in cells {
        if !first {
            write!(writer, ",")?;
        }
        let needs_quote = c.contains(',') || c.contains('"') || c.contains('\n');
        if needs_quote {
            write!(writer, "\"{}\"", c.replace('"', "\"\""))?;
        } else {
            write!(writer, "{c}")?;
        }
        first = false;
    }
    writeln!(writer)?;
    Ok(())
}

fn batch_to_rows(batch: &RecordBatch) -> anyhow::Result<Vec<serde_json::Value>> {
    let schema = batch.schema();
    let mut rows: Vec<serde_json::Map<String, serde_json::Value>> = (0..batch.num_rows())
        .map(|_| serde_json::Map::new())
        .collect();
    let opts = FormatOptions::default();
    for (col_idx, field) in schema.fields().iter().enumerate() {
        let array = batch.column(col_idx);
        let f = ArrayFormatter::try_new(array.as_ref(), &opts)?;
        for (row_idx, row_map) in rows.iter_mut().enumerate() {
            let value = if array.is_null(row_idx) {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(format!("{}", f.value(row_idx)))
            };
            row_map.insert(field.name().clone(), value);
        }
    }
    Ok(rows.into_iter().map(serde_json::Value::Object).collect())
}

/// Optional helper exported for tests.
#[must_use]
pub fn _schema_unused(_s: &Arc<Schema>) -> bool {
    false
}
