//! `pawrly validate` — load + validate `pawrly.yaml`.

use std::path::PathBuf;

use clap::Args as ClapArgs;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Path to validate. Defaults to `--config` / `PAWRLY_CONFIG`, then
    /// ./pawrly.yaml.
    pub path: Option<PathBuf>,
}

pub async fn run(config: Option<PathBuf>, args: Args) -> anyhow::Result<()> {
    let path = args
        .path
        .or(config)
        .unwrap_or_else(|| PathBuf::from("./pawrly.yaml"));
    let cfg = pawrly_config::load_auto(&path)?;
    let errs = pawrly_config::validate(&cfg);
    if errs.is_empty() {
        println!(
            "ok: {} sources, {} tables",
            cfg.sources.len(),
            cfg.sources.iter().map(|s| s.tables.len()).sum::<usize>()
        );
        Ok(())
    } else {
        for e in &errs.0 {
            eprintln!("error: {e}");
        }
        anyhow::bail!("{} validation errors", errs.0.len());
    }
}
