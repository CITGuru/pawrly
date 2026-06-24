//! Build the engine handle the CLI talks to.
//!
//! Decision tree:
//!
//! 1. `--remote <ENDPOINT>` set → connect remotely.
//! 2. `--no-remote` set → run in-process.
//! 3. Default UDS at `$PAWRLY_HOME/sockets/pawrly.sock` exists and is healthy → use it.
//! 4. Otherwise → run in-process.

use std::path::PathBuf;
use std::sync::Arc;

use pawrly_client::{Endpoint, RemoteEngineClient, TlsConfig};
use pawrly_core::EngineService;
use pawrly_engine::{LocalEngine, LocalEngineConfig};

/// Resolve the `--remote` / `--no-remote` flags + auto-discovery and return
/// an `EngineService` handle.
pub async fn build_engine(
    remote: Option<String>,
    no_remote: bool,
    home: Option<PathBuf>,
    config_path: Option<PathBuf>,
) -> anyhow::Result<Arc<dyn EngineService>> {
    if let Some(endpoint) = remote.clone() {
        if endpoint.eq_ignore_ascii_case("off") {
            return build_local(config_path, home).await;
        }
        let ep = parse_endpoint(&endpoint)?;
        let client = RemoteEngineClient::connect(ep).await?;
        return Ok(Arc::new(client));
    }

    if no_remote {
        return build_local(config_path, home).await;
    }

    if let Some(socket) = autodetect_socket(home.as_deref()) {
        if let Ok(client) = RemoteEngineClient::connect(Endpoint::Uds { path: socket }).await
            && client.health().await.is_ok()
        {
            return Ok(Arc::new(client));
        }
    }

    build_local(config_path, home).await
}

/// Build an in-process `LocalEngine` from the resolved config path.
pub async fn build_local(
    config_path: Option<PathBuf>,
    home: Option<PathBuf>,
) -> anyhow::Result<Arc<dyn EngineService>> {
    let path = config_path.or_else(|| default_config_path(home.as_deref()));
    if let Some(p) = path
        && p.exists()
    {
        let engine = LocalEngine::from_config_file_with_home(&p, home).await?;
        return Ok(Arc::new(engine));
    }
    // No manifest anywhere: an empty default workspace, rooted at the Pawrly
    // home so its cache namespace is `default` (there are no relative source
    // paths to anchor).
    let workspace_dir =
        pawrly_core::resolve_home(home.as_deref()).map_or_else(std::env::current_dir, Ok)?;
    let cfg = pawrly_config::Config {
        version: 1,
        name: "default".into(),
        defaults: Default::default(),
        secrets: Vec::new(),
        include: Vec::new(),
        sources: Vec::new(),
        functions: Vec::new(),
        semantic: None,
        observability: None,
    };
    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg,
        workspace_dir,
        duckdb_pool_size: None,
        home,
    })
    .await?;
    Ok(Arc::new(engine))
}

/// Engine-less placeholder used by `pawrly serve` when given no config.
pub async fn local_engine_placeholder() -> anyhow::Result<Arc<dyn EngineService>> {
    let dir = std::env::current_dir()?;
    let engine = LocalEngine::empty(dir).await?;
    Ok(Arc::new(engine))
}

/// Discover the workspace manifest: `$PAWRLY_CONFIG` → `./pawrly.yaml` →
/// `<home>/pawrly.yaml` (the *default workspace*, where `<home>` is `--home` /
/// `$PAWRLY_HOME` / `~/.pawrly`). `--config` is handled by clap before this
/// runs.
pub(crate) fn default_config_path(home: Option<&std::path::Path>) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PAWRLY_CONFIG") {
        return Some(PathBuf::from(p));
    }
    let cwd = std::env::current_dir().ok()?.join("pawrly.yaml");
    if cwd.exists() {
        return Some(cwd);
    }
    let h = pawrly_core::resolve_home(home)?.join("pawrly.yaml");
    if h.exists() { Some(h) } else { None }
}

/// Parse `uds:///path`, `tcp://host:port`, etc.
pub fn parse_endpoint(s: &str) -> anyhow::Result<Endpoint> {
    if let Some(rest) = s
        .strip_prefix("uds://")
        .or_else(|| s.strip_prefix("unix://"))
    {
        let path = if let Some(rest) = rest.strip_prefix('/') {
            PathBuf::from(format!("/{rest}"))
        } else {
            PathBuf::from(rest)
        };
        Ok(Endpoint::Uds { path })
    } else if let Some((rest, secure)) = s
        .strip_prefix("tcps://")
        .map(|r| (r, true))
        .or_else(|| s.strip_prefix("tcp://").map(|r| (r, false)))
    {
        let addr: std::net::SocketAddr = rest
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid tcp endpoint `{rest}`: {e}"))?;
        Ok(Endpoint::Tcp {
            addr,
            bearer: std::env::var("PAWRLY_API_TOKEN").ok(),
            tls: secure.then(tls_config_from_env),
        })
    } else {
        Err(anyhow::anyhow!(
            "unrecognized endpoint `{s}`; use `uds:///path/to/sock`, `tcp://host:port`, or `tcps://host:port` (TLS)"
        ))
    }
}

/// Build the client TLS config from `PAWRLY_TLS_*` environment variables. With
/// no `PAWRLY_TLS_CA`, verification falls back to tonic's compiled-in roots.
fn tls_config_from_env() -> TlsConfig {
    TlsConfig {
        ca_cert: std::env::var_os("PAWRLY_TLS_CA").map(PathBuf::from),
        client_cert: std::env::var_os("PAWRLY_TLS_CLIENT_CERT").map(PathBuf::from),
        client_key: std::env::var_os("PAWRLY_TLS_CLIENT_KEY").map(PathBuf::from),
        domain_name: std::env::var("PAWRLY_TLS_DOMAIN").ok(),
    }
}

fn autodetect_socket(home: Option<&std::path::Path>) -> Option<PathBuf> {
    let candidate = pawrly_core::resolve_home(home)?
        .join("sockets")
        .join("pawrly.sock");
    if candidate.exists() {
        Some(candidate)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_uds_endpoint() {
        let ep = parse_endpoint("uds:///tmp/pawrly.sock").unwrap();
        match ep {
            Endpoint::Uds { path } => assert_eq!(path, PathBuf::from("/tmp/pawrly.sock")),
            _ => panic!("expected UDS endpoint"),
        }
    }

    #[test]
    fn parses_tcp_endpoint() {
        let ep = parse_endpoint("tcp://127.0.0.1:8090").unwrap();
        match ep {
            Endpoint::Tcp { addr, .. } => assert_eq!(addr.port(), 8090),
            _ => panic!("expected TCP endpoint"),
        }
    }

    #[test]
    fn parses_tcps_endpoint_with_tls() {
        let ep = parse_endpoint("tcps://127.0.0.1:8443").unwrap();
        match ep {
            Endpoint::Tcp { addr, tls, .. } => {
                assert_eq!(addr.port(), 8443);
                assert!(tls.is_some(), "tcps:// must enable TLS");
            }
            _ => panic!("expected TCP endpoint"),
        }
    }

    #[test]
    fn plain_tcp_has_no_tls() {
        match parse_endpoint("tcp://127.0.0.1:8090").unwrap() {
            Endpoint::Tcp { tls, .. } => assert!(tls.is_none()),
            _ => panic!("expected TCP endpoint"),
        }
    }

    #[test]
    fn rejects_unknown_scheme() {
        assert!(parse_endpoint("ftp://x").is_err());
    }
}
