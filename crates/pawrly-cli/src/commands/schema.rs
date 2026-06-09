//! `pawrly schema` — inspect the catalog.

use std::path::PathBuf;

use clap::Args as ClapArgs;
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
            for c in &desc.columns {
                let null = if c.nullable { "" } else { " NOT NULL" };
                println!("  {} {}{null}", c.name, c.data_type);
            }
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
        println!("{:<20} {:<10} description", "table", "kind");
        for t in tables {
            println!(
                "{:<20} {:<10} {}",
                t.name,
                t.kind,
                t.description.unwrap_or_default()
            );
        }
    }

    Ok(())
}
