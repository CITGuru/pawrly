//! `pawrly schema` — inspect the catalog.

use std::path::PathBuf;

use clap::{Args as ClapArgs, Subcommand};
use comfy_table::{ContentArrangement, Table};
use pawrly_core::TableName;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Sub-action. With none, list tables (or describe one via `TABLE`).
    #[command(subcommand)]
    pub command: Option<SchemaCommand>,

    /// Describe a single table (`schema.table`). Ignored when a subcommand runs.
    #[arg(value_name = "TABLE")]
    pub table: Option<String>,

    /// Emit JSON instead of a table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Subcommand, Debug)]
pub enum SchemaCommand {
    /// Full catalog snapshot (bulk introspection).
    Snapshot(SnapshotArgs),
}

#[derive(ClapArgs, Debug)]
pub struct SnapshotArgs {
    /// Comma-separated source names to scope the snapshot. Absent = all.
    #[arg(long)]
    pub sources: Option<String>,

    /// Drop per-column detail.
    #[arg(long)]
    pub compact: bool,

    /// Emit JSON instead of a text listing.
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
    if let Some(SchemaCommand::Snapshot(a)) = args.command {
        return snapshot(home, config, remote, no_remote, a).await;
    }

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

async fn snapshot(
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    remote: Option<String>,
    no_remote: bool,
    args: SnapshotArgs,
) -> anyhow::Result<()> {
    let svc = crate::engine::build_engine(remote, no_remote, home, config).await?;
    let sources = args.sources.as_deref().map(|s| {
        s.split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect::<Vec<_>>()
    });
    let snapshot = svc.schema_snapshot(sources, args.compact).await?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
        return Ok(());
    }

    for schema in &snapshot.schemas {
        println!(
            "{} ({}, {} table{})",
            schema.name,
            schema.kind,
            schema.tables.len(),
            if schema.tables.len() == 1 { "" } else { "s" }
        );
        for t in &schema.tables {
            if args.compact {
                println!("  {}", t.name);
            } else {
                println!("  {:<28} {}", t.name, t.columns);
            }
        }
    }
    Ok(())
}
