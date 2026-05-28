//! `pawrly validate` — load + validate `pawrly.yaml`.

use std::path::PathBuf;

use clap::Args as ClapArgs;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Path to validate. Defaults to ./pawrly.yaml.
    #[arg(default_value = "./pawrly.yaml")]
    pub path: PathBuf,
}

pub async fn run(args: Args) -> anyhow::Result<()> {
    let secrets = pawrly_secrets::default_chain();
    let cfg = pawrly_config::load(&args.path, &secrets)?;
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
