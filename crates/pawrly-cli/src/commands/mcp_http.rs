//! `pawrly mcp-http` — run the MCP server over HTTP.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Args as ClapArgs;

use crate::commands::serve::resolve_bearer_token;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Address to bind. A non-loopback address requires `--bearer-token-from`.
    #[arg(long, default_value = "127.0.0.1:8090")]
    pub addr: SocketAddr,

    /// Name of the bearer token to require — resolved from the config's secret
    /// backend, or an environment variable of the same name. Enforced on every
    /// request.
    #[arg(long)]
    pub bearer_token_from: Option<String>,
}

pub async fn run(
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    remote: Option<String>,
    no_remote: bool,
    args: Args,
) -> anyhow::Result<()> {
    let bearer_token = match &args.bearer_token_from {
        Some(name) => Some(resolve_bearer_token(name, config.as_deref())?),
        None => None,
    };
    let engine = crate::engine::build_engine(remote, no_remote, home, config).await?;
    let opts = pawrly_mcp::HttpOpts {
        addr: args.addr,
        bearer_token,
    };
    pawrly_mcp::serve_http(engine, opts).await?;
    Ok(())
}
