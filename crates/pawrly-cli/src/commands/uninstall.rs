//! `pawrly uninstall` — remove the installed `pawrly` binary.
//!
//! By default this removes only the running executable. Pass `--purge` to also
//! delete the Pawrly home directory (`$PAWRLY_HOME` / `~/.pawrly`), which holds
//! the cache, materialized tables, and daemon state. Project config files such
//! as `pawrly.yaml` are never touched.

use std::io::Write;
use std::path::PathBuf;

use anyhow::Context;
use clap::Args as ClapArgs;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Skip the confirmation prompt.
    #[arg(short = 'y', long)]
    pub yes: bool,

    /// Also remove the Pawrly home directory (cache, materialized tables,
    /// daemon state).
    #[arg(long)]
    pub purge: bool,
}

pub async fn run(home: Option<PathBuf>, args: Args) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let home_dir = pawrly_core::resolve_home(home.as_deref());

    println!("This will remove:");
    println!("  binary: {}", exe.display());
    if args.purge {
        match &home_dir {
            Some(dir) if dir.exists() => println!(
                "  data:   {} (cache, materialized tables, daemon state)",
                dir.display()
            ),
            _ => println!("  data:   (none found)"),
        }
    }

    if !args.yes && !confirm("Continue?")? {
        println!("aborted");
        return Ok(());
    }

    // Remove the data directory before the binary, so a failure partway leaves
    // a working `pawrly` to retry with.
    if args.purge
        && let Some(dir) = &home_dir
        && dir.exists()
    {
        std::fs::remove_dir_all(dir)
            .with_context(|| format!("failed to remove data directory {}", dir.display()))?;
        println!("removed {}", dir.display());
    }

    std::fs::remove_file(&exe).with_context(|| format!("failed to remove {}", exe.display()))?;
    println!("removed {}", exe.display());

    if !args.purge
        && let Some(dir) = &home_dir
        && dir.exists()
    {
        println!(
            "note: data left in {} (remove it with `rm -rf {}`)",
            dir.display(),
            dir.display()
        );
    }

    println!("pawrly has been uninstalled");
    Ok(())
}

/// Read a yes/no answer from stdin. A closed or empty stdin counts as "no", so
/// a non-interactive invocation never deletes without `--yes`.
fn confirm(prompt: &str) -> anyhow::Result<bool> {
    print!("{prompt} [y/N] ");
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let answer = input.trim().to_ascii_lowercase();
    Ok(answer == "y" || answer == "yes")
}
