//! Internal task runner. Run with `cargo xtask <command>`.

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "xtask", about = "Pawrly developer tasks")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Generate the JSON schema for `pawrly.yaml`.
    Schema,
    /// Regenerate tonic protobuf bindings.
    Proto,
    /// Generate test fixtures for source crates.
    Fixtures,
    /// Generate the error-code documentation.
    ErrorCodes,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Schema => gen_schema(),
        Command::Proto => {
            println!("xtask proto: not yet implemented.");
            Ok(())
        }
        Command::Fixtures => gen_fixtures(),
        Command::ErrorCodes => {
            println!("xtask error-codes: not yet implemented.");
            Ok(())
        }
    }
}

fn gen_fixtures() -> anyhow::Result<()> {
    use std::sync::Arc;

    use arrow_array::{Int64Array, RecordBatch, StringArray};
    use arrow_schema::{DataType, Field, Schema};
    use parquet::arrow::ArrowWriter;

    let workspace_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| anyhow::anyhow!("could not locate workspace root"))?;

    let dest_dir = workspace_root
        .join("crates")
        .join("pawrly-cli")
        .join("tests")
        .join("fixtures");
    std::fs::create_dir_all(&dest_dir)?;

    // orders.parquet — 5 rows
    {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("customer", DataType::Utf8, true),
            Field::new("amount_cents", DataType::Int64, false),
        ]));
        let id = Arc::new(Int64Array::from(vec![1, 2, 3, 4, 5]));
        let customer = Arc::new(StringArray::from(vec![
            "acme", "ben", "ben", "delta", "echo",
        ]));
        let amount = Arc::new(Int64Array::from(vec![1000_i64, 2500, 4200, 50, 999]));
        let batch = RecordBatch::try_new(schema.clone(), vec![id, customer, amount])?;

        let path = dest_dir.join("orders.parquet");
        let file = std::fs::File::create(&path)?;
        let mut writer = ArrowWriter::try_new(file, schema, None)?;
        writer.write(&batch)?;
        writer.close()?;
        println!("wrote {}", path.display());
    }

    // customers.csv — 3 rows
    {
        let path = dest_dir.join("customers.csv");
        std::fs::write(
            &path,
            "id,name,plan\n1,Acme Corp,enterprise\n2,Ben LLC,team\n3,Delta Co,starter\n",
        )?;
        println!("wrote {}", path.display());
    }

    // events.json (NDJSON) — 4 rows
    {
        let path = dest_dir.join("events.json");
        std::fs::write(
            &path,
            r#"{"id":1,"event":"login","ts":"2026-01-01T00:00:00Z"}
{"id":2,"event":"signup","ts":"2026-01-02T00:00:00Z"}
{"id":3,"event":"login","ts":"2026-01-03T00:00:00Z"}
{"id":4,"event":"logout","ts":"2026-01-04T00:00:00Z"}
"#,
        )?;
        println!("wrote {}", path.display());
    }

    Ok(())
}

fn gen_schema() -> anyhow::Result<()> {
    let schema = pawrly_config::json_schema();
    let json = serde_json::to_string_pretty(&schema)?;
    let workspace_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| anyhow::anyhow!("could not locate workspace root"))?;
    let out_dir = workspace_root.join("schemas");
    std::fs::create_dir_all(&out_dir)?;
    let out = out_dir.join("pawrly.schema.json");
    std::fs::write(&out, json)?;
    println!("wrote {}", out.display());
    Ok(())
}
