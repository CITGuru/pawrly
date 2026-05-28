//! `pawrly init` — create a starter `pawrly.yaml` in the current directory.

use std::path::PathBuf;

use clap::Args as ClapArgs;

const STARTER: &str = "# Pawrly workspace config.\n\
                       \n\
                       version: 1\n\
                       name: default\n\
                       \n\
                       defaults:\n\
                         cache:\n\
                           storage: ~/.pawrly/cache\n\
                           mode: { mode: none }\n\
                         safety:\n\
                           max_unfiltered_rows: 1000000\n\
                       \n\
                       sources: []\n\
                       \n\
                       # Add a source with: pawrly source add <kind> --name <name>\n";

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Path to write. Defaults to ./pawrly.yaml.
    #[arg(default_value = "./pawrly.yaml")]
    pub path: PathBuf,

    /// Overwrite an existing file.
    #[arg(long)]
    pub force: bool,
}

pub async fn run(args: Args) -> anyhow::Result<()> {
    if args.path.exists() && !args.force {
        anyhow::bail!(
            "{} already exists; pass --force to overwrite",
            args.path.display()
        );
    }
    if let Some(parent) = args.path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&args.path, STARTER)?;
    println!("wrote {}", args.path.display());
    Ok(())
}
