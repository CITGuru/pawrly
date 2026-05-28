//! `pawrly mcp-stdio` — run the MCP server over stdio.

use std::path::PathBuf;

use clap::Args as ClapArgs;

#[derive(ClapArgs, Debug)]
pub struct Args {}

pub async fn run(
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    remote: Option<String>,
    no_remote: bool,
    _args: Args,
) -> anyhow::Result<()> {
    let engine = crate::engine::build_engine(remote, no_remote, home, config).await?;
    pawrly_mcp::serve_stdio(engine).await?;
    Ok(())
}
