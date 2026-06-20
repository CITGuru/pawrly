//! `pawrly schema` — inspect the catalog.

use std::path::PathBuf;

use clap::Args as ClapArgs;
use comfy_table::{ContentArrangement, Table};
use pawrly_core::TableName;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// If set, describe a single table (`schema.table`).
    #[arg(value_name = "TABLE")]
    pub table: Option<String>,

    /// Emit JSON instead of a table.
    #[arg(long)]
    pub json: bool,
}

pub async fn run(
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    remote: Option<String>,
    no_remote: bool,
    args: Args,
) -> anyhow::Result<()> {
    let svc = crate::engine::build_engine(remote, no_remote, home, config).await?;

    if let Some(name) = args.table.as_deref() {
        let n = TableName::parse(name)
            .ok_or_else(|| anyhow::anyhow!("expected `schema.table`, got `{name}`"))?;
        let desc = svc.describe_table(&n).await?;
        if args.json {
            println!("{}", serde_json::to_string_pretty(&desc)?);
        } else {
            println!("{}: {}", desc.table.name, desc.table.kind);
            let mut table = Table::new();
            table.set_content_arrangement(ContentArrangement::Dynamic);
            table.set_header(vec!["column", "type", "nullable"]);
            for c in &desc.columns {
                table.add_row(vec![
                    c.name.clone(),
                    c.data_type.to_string(),
                    if c.nullable { "yes" } else { "no" }.to_string(),
                ]);
            }
            println!("{table}");
            if let Some(wiki) = &desc.wiki {
                println!("\nnotes:\n{wiki}");
            }
        }
        return Ok(());
    }

    let tables = svc.list_tables(None).await?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&tables)?);
    } else {
        let mut table = Table::new();
        table.set_content_arrangement(ContentArrangement::Dynamic);
        table.set_header(vec!["table", "kind", "description"]);
        for t in tables {
            table.add_row(vec![
                t.name.to_string(),
                t.kind.to_string(),
                t.description.unwrap_or_default(),
            ]);
        }
        println!("{table}");
    }

    Ok(())
}
