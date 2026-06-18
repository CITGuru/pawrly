//! `pawrly console` — serve the web Console (gRPC-Web + assets) in-process.
//!
//! The same serving path as `pawrly serve --console`: it resolves the workspace
//! like the rest of the CLI (`build_engine`, honoring `--remote` / `--config` /
//! `--home`), binds TCP, speaks gRPC-Web, and serves the embedded SPA (with the
//! `console` feature).

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Args as ClapArgs;
use pawrly_server::{AuthMode, ConsoleOpts, ServerBuilder};

use crate::commands::serve::resolve_bearer_token;
use crate::engine::{build_engine, default_config_path};

/// Default loopback bind for the Console.
fn default_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 8787))
}

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// TCP address to bind. Defaults to `127.0.0.1:8787` (loopback → no token
    /// or CORS needed).
    #[arg(long)]
    pub addr: Option<SocketAddr>,

    /// Name of the bearer token to require — resolved from the config's secret
    /// backend or an environment variable of the same name. Required for a
    /// non-loopback bind; sent by the browser as gRPC-Web metadata.
    #[arg(long)]
    pub bearer_token_from: Option<String>,

    /// Allow this browser origin (standalone / cross-origin hosting), e.g.
    /// `https://console.example.com`. Omit for same-origin embedded mode.
    #[arg(long)]
    pub cors_origin: Option<String>,
}

pub async fn run(
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    remote: Option<String>,
    no_remote: bool,
    args: Args,
) -> anyhow::Result<()> {
    // Same manifest discovery as the rest of the CLI so the secret backend that
    // resolves the bearer token matches the workspace being served.
    let config = config.or_else(|| default_config_path(home.as_deref()));

    let auth_token = match &args.bearer_token_from {
        Some(name) => Some(resolve_bearer_token(name, config.as_deref())?),
        None => None,
    };

    let engine = build_engine(remote, no_remote, home, config).await?;
    let mut builder = ServerBuilder::new(engine);
    if let Some(token) = auth_token {
        builder = builder.auth(AuthMode::Bearer { token });
    }

    let addr = args.addr.unwrap_or_else(default_addr);
    tracing::info!(%addr, "starting pawrly console");
    builder
        .serve_console(ConsoleOpts {
            addr,
            cors_origin: args.cors_origin,
        })
        .await?;
    Ok(())
}
