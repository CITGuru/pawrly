//! `pawrly config` — inspect the workspace config.
//!
//! `show` prints the config after `include:` / `from:` assembly. Secrets are
//! **not** resolved here, so `${secret:…}` references are shown verbatim (the
//! masked form) and never leak. `--raw` skips assembly too; `--tree` prints the
//! include graph.

use std::path::{Path, PathBuf};

use clap::{Args as ClapArgs, Subcommand};

use pawrly_config::IncludeNode;

#[derive(ClapArgs, Debug)]
pub struct Args {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Print the resolved workspace config.
    Show(ShowArgs),
    /// Re-read the workspace config into the running engine.
    Reload(ReloadArgs),
}

#[derive(ClapArgs, Debug)]
pub struct ShowArgs {
    /// Print the root file verbatim — no include/from assembly, no interpolation.
    #[arg(long, conflicts_with = "tree")]
    pub raw: bool,

    /// Print the include graph (root → fragments) as a tree.
    #[arg(long)]
    pub tree: bool,
}

#[derive(ClapArgs, Debug)]
pub struct ReloadArgs {
    /// Emit JSON instead of plain text.
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
        ConfigCommand::Show(a) => run_show(config, a),
        ConfigCommand::Reload(a) => run_reload(home, config, remote, no_remote, a).await,
    }
}

async fn run_reload(
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    remote: Option<String>,
    no_remote: bool,
    args: ReloadArgs,
) -> anyhow::Result<()> {
    let svc = crate::engine::build_engine(remote, no_remote, home, config).await?;
    let report = svc.reload_config().await?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "reloaded: {} added, {} removed, {} changed",
            report.sources_added, report.sources_removed, report.sources_changed
        );
    }
    Ok(())
}

fn run_show(config: Option<PathBuf>, args: ShowArgs) -> anyhow::Result<()> {
    let path = resolve_config_path(config)?;
    if !path.exists() {
        anyhow::bail!("config not found: {}", path.display());
    }

    if args.raw {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
        print!("{raw}");
        return Ok(());
    }

    if args.tree {
        let root = pawrly_config::include_tree(&path)?;
        print_tree(&root);
        return Ok(());
    }

    // Default: assembled config with secret references preserved verbatim.
    let (cfg, _origins) = pawrly_config::assemble_config(&path)?;
    let yaml = serde_yaml::to_string(&cfg).map_err(|e| anyhow::anyhow!("serialize config: {e}"))?;
    print!("{yaml}");
    Ok(())
}

/// Resolve which `pawrly.yaml` to inspect. Falls back to `./pawrly.yaml`.
fn resolve_config_path(explicit: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p);
    }
    if let Ok(p) = std::env::var("PAWRLY_CONFIG") {
        return Ok(PathBuf::from(p));
    }
    Ok(std::env::current_dir()?.join("pawrly.yaml"))
}

fn print_tree(root: &IncludeNode) {
    let root_dir = root.path.parent().unwrap_or_else(|| Path::new("."));
    println!("{}", root.path.display());
    let last = root.children.len();
    for (i, child) in root.children.iter().enumerate() {
        print_node(child, root_dir, "", i + 1 == last);
    }
}

fn print_node(node: &IncludeNode, root_dir: &Path, prefix: &str, is_last: bool) {
    let branch = if is_last { "└─ " } else { "├─ " };
    let label = node
        .path
        .strip_prefix(root_dir)
        .unwrap_or(&node.path)
        .display();
    println!("{prefix}{branch}{label}");

    let child_prefix = format!("{prefix}{}", if is_last { "   " } else { "│  " });
    let last = node.children.len();
    for (i, child) in node.children.iter().enumerate() {
        print_node(child, root_dir, &child_prefix, i + 1 == last);
    }
}
