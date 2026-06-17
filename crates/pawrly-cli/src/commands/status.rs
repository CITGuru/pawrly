//! `pawrly status` — show running daemons.

use std::path::PathBuf;
use std::sync::Arc;

use clap::Args as ClapArgs;
use pawrly_client::{ConnectError, Endpoint, RemoteEngineClient};
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
    // `default_socket` is `Some(path)` only when probing the implicit default
    // socket (no `--endpoint`). For that case a refused connection means a
    // stale socket left by a daemon that exited uncleanly, which we report as
    // "not running" rather than a raw transport error.
    let (endpoint, default_socket) = match args.endpoint {
        Some(s) => (crate::engine::parse_endpoint(&s)?, None),
        None => {
            let path = default_socket_path(home.as_deref())?;
            if !path.exists() {
                print_not_running(
                    args.json,
                    &format!("default socket `{}` not found", path.display()),
                );
                return Ok(());
            }
            (Endpoint::Uds { path: path.clone() }, Some(path))
        }
    };

    let client = match RemoteEngineClient::connect(endpoint).await {
        Ok(client) => client,
        Err(err) => {
            if let Some(path) = default_socket {
                if is_connection_refused(&err) {
                    // Daemon is gone but left its socket behind; clean it up so
                    // the next probe takes the fast `!path.exists()` path.
                    let _ = std::fs::remove_file(&path);
                    print_not_running(
                        args.json,
                        &format!("removed stale socket `{}`", path.display()),
                    );
                    return Ok(());
                }
            }
            return Err(err.into());
        }
    };
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

/// Print the "no daemon running" result in the requested format. `note`
/// explains why (text output only); JSON stays a stable machine-readable shape.
fn print_not_running(json: bool, note: &str) {
    if json {
        println!("{{\"running\": false}}");
    } else {
        println!("no daemon running ({note})");
    }
}

/// Walk the error's source chain looking for an [`std::io::Error`] reporting a
/// refused or vanished connection — the signature of a stale Unix socket whose
/// daemon is gone. The transport layer wraps this in opaque tonic/hyper errors,
/// so a raw string match would be brittle.
fn is_connection_refused(err: &ConnectError) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(cause) = source {
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            if matches!(
                io.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) {
                return true;
            }
        }
        source = cause.source();
    }
    false
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
