//! `pawrly cache` — inspect and manage the cache.
//!
//! Every subcommand is wired through `EngineService`: `list`/`show` read the
//! manifest, `refresh` re-fetches a table (or a source's catalog),
//! `invalidate` drops an entry, and `vacuum` reclaims space.

use std::path::PathBuf;

use clap::{Args as ClapArgs, Subcommand};
use pawrly_core::TableName;

#[derive(ClapArgs, Debug)]
pub struct Args {
    #[command(subcommand)]
    pub command: CacheCommand,
}

#[derive(Subcommand, Debug)]
pub enum CacheCommand {
    /// List cache entries with mode, freshness, rows, and size.
    List(ListArgs),
    /// Show a detailed view of a single cache entry.
    Show(ShowArgs),
    /// Refresh a table (`<source>.<table>`) or a source's catalog.
    Refresh(RefreshArgs),
    /// Drop a cache entry and its files.
    Invalidate(InvalidateArgs),
    /// Reclaim space from expired entries, orphaned files, and stale temp writes.
    Vacuum(VacuumArgs),
}

#[derive(ClapArgs, Debug)]
pub struct ListArgs {
    /// Emit JSON instead of a table.
    #[arg(long)]
    pub json: bool,
}

#[derive(ClapArgs, Debug)]
pub struct ShowArgs {
    /// `schema.table` of the cache entry to show.
    #[arg(value_name = "ID")]
    pub id: String,
}

#[derive(ClapArgs, Debug)]
pub struct RefreshArgs {
    /// `<source>.<table>` to refresh one table, or a source name to refresh its catalog.
    #[arg(value_name = "NAME")]
    pub name: String,
}

#[derive(ClapArgs, Debug)]
pub struct InvalidateArgs {
    /// `schema.table` of the cache entry to invalidate.
    #[arg(value_name = "ID")]
    pub id: String,
}

#[derive(ClapArgs, Debug)]
pub struct VacuumArgs {
    /// Emit JSON instead of a human-readable report.
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
    match args.command {
        CacheCommand::List(a) => list(home, config, remote, no_remote, a).await,
        CacheCommand::Show(a) => show(home, config, remote, no_remote, a).await,
        CacheCommand::Refresh(a) => refresh(home, config, remote, no_remote, a).await,
        CacheCommand::Invalidate(a) => invalidate(home, config, remote, no_remote, a).await,
        CacheCommand::Vacuum(a) => vacuum(home, config, remote, no_remote, a).await,
    }
}

async fn list(
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    remote: Option<String>,
    no_remote: bool,
    args: ListArgs,
) -> anyhow::Result<()> {
    let svc = crate::engine::build_engine(remote, no_remote, home, config).await?;
    let entries = svc.cache_entries().await?;

    if args.json {
        println!("{}", serde_json::to_string(&entries)?);
        return Ok(());
    }

    if entries.is_empty() {
        println!("no cache entries");
        return Ok(());
    }

    println!(
        "{:<24} {:<8} {:<10} {:<12} {:<12} written_at",
        "table", "mode", "rows", "size_bytes", "files"
    );
    for e in &entries {
        let mode = format!("{:?}", e.mode).to_lowercase();
        let name = e.name.to_string();
        println!(
            "{:<24} {:<8} {:<10} {:<12} {:<12} {}",
            name, mode, e.row_count, e.size_bytes, e.file_count, e.written_at
        );
    }
    Ok(())
}

async fn show(
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    remote: Option<String>,
    no_remote: bool,
    args: ShowArgs,
) -> anyhow::Result<()> {
    let name = TableName::parse(&args.id)
        .ok_or_else(|| anyhow::anyhow!("invalid id `{}`; expected `<source>.<table>`", args.id))?;
    let svc = crate::engine::build_engine(remote, no_remote, home, config).await?;
    let entries = svc.cache_entries().await?;
    let Some(e) = entries.into_iter().find(|e| e.name == name) else {
        println!("no cache entry for {name}");
        return Ok(());
    };
    let mode = format!("{:?}", e.mode).to_lowercase();
    println!("table:       {}", e.name);
    println!("mode:        {mode}");
    println!("rows:        {}", e.row_count);
    println!("size_bytes:  {}", e.size_bytes);
    println!("files:       {}", e.file_count);
    println!("written_at:  {}", e.written_at);
    match e.expires_at {
        Some(exp) => println!("expires_at:  {exp}"),
        None => println!("expires_at:  never"),
    }
    Ok(())
}

async fn refresh(
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    remote: Option<String>,
    no_remote: bool,
    args: RefreshArgs,
) -> anyhow::Result<()> {
    let svc = crate::engine::build_engine(remote, no_remote, home, config).await?;

    // `<source>.<table>` refreshes one cached table; a bare name refreshes the
    // source's catalog.
    if let Some(name) = TableName::parse(&args.name) {
        let out = svc.refresh_table(&name).await?;
        println!(
            "refreshed {}: {} rows, {} bytes in {:?}",
            name, out.rows_written, out.size_bytes, out.elapsed
        );
        return Ok(());
    }

    let outcome = svc.refresh_catalog(Some(&args.name)).await?;
    println!(
        "refreshed {} sources, {} tables discovered",
        outcome.sources_refreshed, outcome.tables_discovered
    );
    Ok(())
}

async fn invalidate(
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    remote: Option<String>,
    no_remote: bool,
    args: InvalidateArgs,
) -> anyhow::Result<()> {
    let name = TableName::parse(&args.id)
        .ok_or_else(|| anyhow::anyhow!("invalid id `{}`; expected `<source>.<table>`", args.id))?;
    let svc = crate::engine::build_engine(remote, no_remote, home, config).await?;
    if svc.invalidate_cache(&name).await? {
        println!("invalidated {name}");
    } else {
        println!("no cache entry for {name}");
    }
    Ok(())
}

async fn vacuum(
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    remote: Option<String>,
    no_remote: bool,
    args: VacuumArgs,
) -> anyhow::Result<()> {
    let svc = crate::engine::build_engine(remote, no_remote, home, config).await?;
    let report = svc.vacuum_cache().await?;
    if args.json {
        println!("{}", serde_json::to_string(&report)?);
        return Ok(());
    }
    println!(
        "removed {} entries, {} files, reclaimed {} bytes",
        report.entries_removed, report.files_removed, report.bytes_reclaimed
    );
    Ok(())
}
