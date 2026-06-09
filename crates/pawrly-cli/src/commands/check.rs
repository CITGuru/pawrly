//! `pawrly check` — run each source's `examples:` statements as live probes,
//! so a broken endpoint or credential is caught now rather than at first query.

use std::path::PathBuf;
use std::time::Instant;

use clap::Args as ClapArgs;
use pawrly_core::EngineServiceExt;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Only check this source.
    #[arg(long)]
    pub source: Option<String>,
}

pub async fn run(
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    remote: Option<String>,
    no_remote: bool,
    args: Args,
) -> anyhow::Result<()> {
    // The examples live in the workspace config; the probes themselves run
    // through whichever engine the global flags select (local or daemon).
    let path = config
        .clone()
        .or_else(crate::engine::default_config_path)
        .filter(|p| p.exists())
        .ok_or_else(|| anyhow::anyhow!("no pawrly.yaml found; pass --config"))?;
    let cfg = pawrly_config::load_auto(&path)?;

    let svc = crate::engine::build_engine(remote, no_remote, home, config).await?;

    let mut passed = 0u64;
    let mut failed = 0u64;
    for src in &cfg.sources {
        if let Some(only) = &args.source {
            if &src.name != only {
                continue;
            }
        }
        if src.examples.is_empty() {
            continue;
        }
        println!("{}:", src.name);
        for sql in &src.examples {
            let start = Instant::now();
            match svc.query_collect(sql).await {
                Ok(_) => {
                    passed += 1;
                    println!("  ok   {sql} ({}ms)", start.elapsed().as_millis());
                }
                Err(e) => {
                    failed += 1;
                    println!("  FAIL {sql}");
                    println!("       {e}");
                }
            }
        }
    }

    if passed == 0 && failed == 0 {
        println!("no examples declared; nothing to check");
        return Ok(());
    }
    println!("{passed} passed, {failed} failed");
    if failed > 0 {
        anyhow::bail!("{failed} example(s) failed");
    }
    Ok(())
}
