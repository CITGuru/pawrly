//! `pawrly status` — show running daemons.

use std::path::PathBuf;
use std::sync::Arc;

use clap::Args as ClapArgs;
use pawrly_client::{Endpoint, RemoteEngineClient};
use pawrly_core::EngineService;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Emit machine-readable JSON.
    #[arg(long)]
    pub json: bool,

    /// Endpoint to probe. Defaults to the default UDS path under `$PAWRLY_HOME`.
    #[arg(long)]
    pub endpoint: Option<String>,
}

pub async fn run(home: Option<PathBuf>, args: Args) -> anyhow::Result<()> {
    let endpoint = match args.endpoint {
        Some(s) => crate::engine::parse_endpoint(&s)?,
        None => {
            let path = default_socket_path(home.as_deref())?;
            if !path.exists() {
                if args.json {
                    println!("{{\"running\": false}}");
                } else {
                    println!(
                        "no daemon running (default socket `{}` not found)",
                        path.display()
                    );
                }
                return Ok(());
            }
            Endpoint::Uds { path }
        }
    };

    let client = RemoteEngineClient::connect(endpoint).await?;
    let svc: Arc<dyn EngineService> = Arc::new(client);
    let h = svc.health().await?;

    if args.json {
        println!(
            "{{\"running\": true, \"version\": \"{}\", \"sources_ok\": {}, \"sources_unavailable\": {}, \"active_queries\": {}}}",
            h.version, h.sources_ok, h.sources_unavailable, h.active_queries
        );
    } else {
        println!(
            "pawrly daemon running: version={} sources_ok={} sources_unavailable={} active_queries={}",
            h.version, h.sources_ok, h.sources_unavailable, h.active_queries
        );
    }

    Ok(())
}

fn default_socket_path(home: Option<&std::path::Path>) -> anyhow::Result<PathBuf> {
    let h = home
        .map(|p| p.to_path_buf())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|h| h.join(".pawrly"))
        })
        .ok_or_else(|| anyhow::anyhow!("could not resolve $PAWRLY_HOME (no $HOME)"))?;
    Ok(h.join("sockets").join("pawrly.sock"))
}
