//! gRPC server for Pawrly. Wraps any [`EngineService`] implementation
//! and exposes it over Unix-domain sockets, TCP, or in-process channels.

#![doc(html_root_url = "https://docs.rs/pawrly-server")]

mod auth;
mod error;
mod services;

pub use auth::{AuthInterceptor, AuthMode};
pub use error::ServerError;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pawrly_core::EngineService;
use pawrly_proto::v1::{
    admin_service_server::AdminServiceServer, cache_service_server::CacheServiceServer,
    catalog_service_server::CatalogServiceServer, query_service_server::QueryServiceServer,
    semantic_service_server::SemanticServiceServer, sources_service_server::SourcesServiceServer,
};
use tonic::transport::server::Router;
use tonic::transport::{Channel, Endpoint, Identity, Server, ServerTlsConfig, Uri};

use crate::services::{AdminSvc, CacheSvc, CatalogSvc, QuerySvc, SemanticSvc, SourcesSvc};

/// Builder for the Pawrly gRPC server.
pub struct ServerBuilder {
    engine: Arc<dyn EngineService>,
    auth: AuthMode,
    /// PEM cert + key paths for TLS, loaded when the router is built.
    tls: Option<(PathBuf, PathBuf)>,
}

impl ServerBuilder {
    /// Construct from an engine implementation.
    #[must_use]
    pub fn new(engine: Arc<dyn EngineService>) -> Self {
        Self {
            engine,
            auth: AuthMode::None,
            tls: None,
        }
    }

    /// Configure authentication. TCP transport on non-loopback addresses
    /// requires `AuthMode::Bearer`.
    #[must_use]
    pub fn auth(mut self, auth: AuthMode) -> Self {
        self.auth = auth;
        self
    }

    /// Serve over TLS, presenting the identity in the given PEM certificate and
    /// private-key files. Applies to TCP transports; UDS already relies on file
    /// permissions as its trust boundary.
    #[must_use]
    pub fn tls(mut self, cert: impl Into<PathBuf>, key: impl Into<PathBuf>) -> Self {
        self.tls = Some((cert.into(), key.into()));
        self
    }

    /// Serve on a Unix domain socket. Creates the socket at `path` with
    /// mode 0600.
    #[cfg(unix)]
    pub async fn serve_uds(self, path: impl AsRef<Path>) -> Result<(), ServerError> {
        let path = path.as_ref();
        // Remove any stale socket from a previous run.
        let _ = tokio::fs::remove_file(path).await;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(ServerError::Io)?;
        }
        let listener = tokio::net::UnixListener::bind(path).map_err(ServerError::Io)?;
        // Tighten permissions to user-only (0600).
        use std::os::unix::fs::PermissionsExt as _;
        let mut perms = std::fs::metadata(path)
            .map_err(ServerError::Io)?
            .permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(path, perms).map_err(ServerError::Io)?;

        tracing::info!(socket = %path.display(), "pawrly-server listening on UDS");
        let stream = tokio_stream::wrappers::UnixListenerStream::new(listener);
        self.router()?
            .serve_with_incoming(stream)
            .await
            .map_err(ServerError::Transport)
    }

    /// Serve on TCP.
    pub async fn serve_tcp(self, addr: std::net::SocketAddr) -> Result<(), ServerError> {
        if !is_loopback(addr.ip()) && matches!(self.auth, AuthMode::None) {
            return Err(ServerError::AuthRequiredForNonLoopback);
        }
        tracing::info!(%addr, "pawrly-server listening on TCP");
        self.router()?
            .serve(addr)
            .await
            .map_err(ServerError::Transport)
    }

    /// Serve on an already-bound TCP listener. Lets the caller pick an ephemeral
    /// port and learn it via [`tokio::net::TcpListener::local_addr`] before
    /// serving (used by tests and socket activation). The non-loopback auth
    /// guard is the caller's responsibility on this path.
    pub async fn serve_tcp_incoming(
        self,
        listener: tokio::net::TcpListener,
    ) -> Result<(), ServerError> {
        let stream = tokio_stream::wrappers::TcpListenerStream::new(listener);
        self.router()?
            .serve_with_incoming(stream)
            .await
            .map_err(ServerError::Transport)
    }

    /// Spawn the server on an in-process duplex channel and return a tonic
    /// `Channel` connected to it. Mainly used for tests.
    pub async fn serve_in_process(self) -> Result<Channel, ServerError> {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let client = Some(client);

        // Spawn the server on the duplex stream.
        let router = self.router()?;
        tokio::spawn(async move {
            let stream = futures::stream::once(async move { Ok::<_, std::io::Error>(server) });
            if let Err(e) = router.serve_with_incoming(stream).await {
                tracing::error!(error = %e, "in-process server exited with error");
            }
        });

        // Connect a tonic Channel to it.
        let mut maybe_client = client;
        let endpoint = Endpoint::try_from("http://[::]:50051").map_err(ServerError::Transport)?;
        let channel = endpoint
            .connect_with_connector(tower::service_fn(move |_: Uri| {
                let c = maybe_client.take();
                async move {
                    c.map(hyper_util::rt::TokioIo::new)
                        .ok_or_else(|| std::io::Error::other("in-process channel already used"))
                }
            }))
            .await
            .map_err(ServerError::Transport)?;
        Ok(channel)
    }

    fn router(self) -> Result<Router, ServerError> {
        // Every service is wrapped with the same auth interceptor; in
        // `AuthMode::None` it is a no-op, otherwise it enforces the bearer token.
        let auth = AuthInterceptor::new(&self.auth);
        let mut builder = Server::builder();
        if let Some((cert, key)) = &self.tls {
            let cert_pem = std::fs::read(cert).map_err(ServerError::Io)?;
            let key_pem = std::fs::read(key).map_err(ServerError::Io)?;
            let identity = Identity::from_pem(cert_pem, key_pem);
            builder = builder
                .tls_config(ServerTlsConfig::new().identity(identity))
                .map_err(ServerError::Transport)?;
        }
        let engine = self.engine;
        Ok(builder
            .add_service(QueryServiceServer::with_interceptor(
                QuerySvc::new(engine.clone()),
                auth.clone(),
            ))
            .add_service(CatalogServiceServer::with_interceptor(
                CatalogSvc::new(engine.clone()),
                auth.clone(),
            ))
            .add_service(SourcesServiceServer::with_interceptor(
                SourcesSvc::new(engine.clone()),
                auth.clone(),
            ))
            .add_service(CacheServiceServer::with_interceptor(
                CacheSvc::new(engine.clone()),
                auth.clone(),
            ))
            .add_service(SemanticServiceServer::with_interceptor(
                SemanticSvc::new(engine.clone()),
                auth.clone(),
            ))
            .add_service(AdminServiceServer::with_interceptor(
                AdminSvc::new(engine),
                auth,
            )))
    }
}

fn is_loopback(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v) => v.is_loopback(),
        std::net::IpAddr::V6(v) => v.is_loopback(),
    }
}
