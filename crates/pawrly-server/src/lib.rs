//! gRPC server for Pawrly. Wraps any [`EngineService`] implementation
//! and exposes it over Unix-domain sockets, TCP, or in-process channels.

#![doc(html_root_url = "https://docs.rs/pawrly-server")]

mod auth;
mod error;
mod rest;
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
use tonic::service::Routes;
use tonic::transport::server::Router;
use tonic::transport::{Channel, Endpoint, Identity, Server, ServerTlsConfig, Uri};
use tonic_web::{GrpcWebLayer, GrpcWebService};
use tower::Layer as _;

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

    /// Serve gRPC-Web (+ optional embedded Console assets and CORS) over TCP for
    /// browser clients. See [`ConsoleOpts`]. Shares [`Self::service_routes`] with
    /// the machine wire ([`Self::router`]) so the two paths can't drift: the only
    /// additions here are the gRPC-Web translation layer, the static-asset
    /// fallback, and (in standalone mode) a scoped CORS layer.
    ///
    /// Unlike the tonic transport paths this serves through axum, which does not
    /// carry tonic's `ServerTlsConfig`; a non-loopback deployment must terminate
    /// TLS in front (or via `axum-server`) so the bearer token does not cross the
    /// wire in cleartext.
    pub async fn serve_console(self, opts: ConsoleOpts) -> Result<(), ServerError> {
        // Same non-loopback guard as `serve_tcp`.
        if !is_loopback(opts.addr.ip()) && matches!(self.auth, AuthMode::None) {
            return Err(ServerError::AuthRequiredForNonLoopback);
        }
        let cors_origin = opts.cors_origin;

        let bearer = match &self.auth {
            AuthMode::None => None,
            AuthMode::Bearer { token } => Some(Arc::from(token.as_str())),
        };
        let rest = rest::rest_router(self.engine.clone(), bearer);

        // `Routes` is itself a `Service<Request<BoxBody>>`, so gRPC-Web layers on
        // directly; one axum fallback splits `/pawrly.v1.*` from static assets.
        let grpc = GrpcWebLayer::new().layer(self.service_routes());
        let console = axum::Router::new()
            .fallback(console_dispatch)
            .with_state(ConsoleState { grpc });
        let app = rest.merge(console);

        let app = match &cors_origin {
            Some(origin) => app.layer(console_cors(origin)?),
            None => app,
        };

        let listener = tokio::net::TcpListener::bind(opts.addr)
            .await
            .map_err(ServerError::Io)?;
        tracing::info!(
            addr = %opts.addr,
            cors = cors_origin.is_some(),
            "pawrly console (gRPC-Web + assets) listening"
        );
        axum::serve(listener, app).await.map_err(ServerError::Io)?;
        Ok(())
    }

    fn router(self) -> Result<Router, ServerError> {
        let mut builder = Server::builder();
        if let Some((cert, key)) = &self.tls {
            let cert_pem = std::fs::read(cert).map_err(ServerError::Io)?;
            let key_pem = std::fs::read(key).map_err(ServerError::Io)?;
            let identity = Identity::from_pem(cert_pem, key_pem);
            builder = builder
                .tls_config(ServerTlsConfig::new().identity(identity))
                .map_err(ServerError::Transport)?;
        }
        Ok(builder.add_routes(self.service_routes()))
    }

    /// The six interceptor-wrapped services, registered once and shared by the
    /// machine wire ([`Self::router`]) and the console wire
    /// ([`Self::serve_console`]) so they can't drift.
    fn service_routes(&self) -> Routes {
        let auth = AuthInterceptor::new(&self.auth);
        let engine = self.engine.clone();
        let mut routes = Routes::builder();
        routes.add_service(QueryServiceServer::with_interceptor(
            QuerySvc::new(engine.clone()),
            auth.clone(),
        ));
        routes.add_service(CatalogServiceServer::with_interceptor(
            CatalogSvc::new(engine.clone()),
            auth.clone(),
        ));
        routes.add_service(SourcesServiceServer::with_interceptor(
            SourcesSvc::new(engine.clone()),
            auth.clone(),
        ));
        routes.add_service(CacheServiceServer::with_interceptor(
            CacheSvc::new(engine.clone()),
            auth.clone(),
        ));
        routes.add_service(SemanticServiceServer::with_interceptor(
            SemanticSvc::new(engine.clone()),
            auth.clone(),
        ));
        routes.add_service(AdminServiceServer::with_interceptor(
            AdminSvc::new(engine),
            auth,
        ));
        routes.routes()
    }
}

fn is_loopback(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v) => v.is_loopback(),
        std::net::IpAddr::V6(v) => v.is_loopback(),
    }
}

/// Options for [`ServerBuilder::serve_console`].
pub struct ConsoleOpts {
    /// TCP address to bind (e.g. `127.0.0.1:8787`).
    pub addr: std::net::SocketAddr,
    /// `Some(origin)` enables a scoped CORS layer for standalone (cross-origin)
    /// hosting; `None` is same-origin embedded mode, which needs no CORS.
    pub cors_origin: Option<String>,
}

/// Shared state for the console fallback handler: the gRPC-Web-wrapped service
/// registry, cloned per request.
#[derive(Clone)]
struct ConsoleState {
    grpc: GrpcWebService<Routes>,
}

/// Single fallback: `/pawrly.v1.*` paths go to the gRPC-Web service (bridging
/// axum's body to tonic's `BoxBody` and back); everything else is served from
/// the embedded SPA.
async fn console_dispatch(
    axum::extract::State(state): axum::extract::State<ConsoleState>,
    req: axum::extract::Request,
) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    use tower::ServiceExt as _;

    if req.uri().path().starts_with("/pawrly.v1.") {
        let req = req.map(tonic::body::boxed);
        match state.grpc.clone().oneshot(req).await {
            Ok(res) => res.map(axum::body::Body::new),
            // `Routes` is infallible in practice; surface anything unexpected.
            Err(err) => {
                tracing::error!(error = %err, "console gRPC-Web dispatch failed");
                http::StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    } else {
        serve_embedded_asset(req.uri().path())
    }
}

/// Build the scoped CORS layer for standalone mode. gRPC-Web sends
/// `x-grpc-web` / `x-user-agent` and returns call status in `grpc-status` /
/// `grpc-message`; a default `CorsLayer` would not allow/expose these and would
/// silently break the browser client.
fn console_cors(origin: &str) -> Result<tower_http::cors::CorsLayer, ServerError> {
    use tower_http::cors::{AllowOrigin, CorsLayer};
    let value: http::HeaderValue = origin
        .parse()
        .map_err(|_| ServerError::InvalidCorsOrigin(origin.to_string()))?;
    Ok(CorsLayer::new()
        .allow_origin(AllowOrigin::exact(value))
        .allow_headers([
            http::header::AUTHORIZATION,
            http::header::CONTENT_TYPE,
            http::HeaderName::from_static("x-grpc-web"),
            http::HeaderName::from_static("x-user-agent"),
            // Console emits a W3C traceparent per call for activity correlation.
            http::HeaderName::from_static("traceparent"),
        ])
        .expose_headers([
            http::HeaderName::from_static("grpc-status"),
            http::HeaderName::from_static("grpc-message"),
        ])
        .allow_methods([http::Method::POST, http::Method::GET, http::Method::OPTIONS]))
}

/// Serve the embedded SPA, with an `index.html` fallback so client-side routing
/// works. Only present when built with the `console` feature; otherwise this is
/// a hint that the assets were not bundled (Mode B can still serve a standalone
/// SPA against the gRPC-Web endpoint).
#[cfg(feature = "console")]
fn serve_embedded_asset(path: &str) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    let path = path.trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match ConsoleAssets::get(path).or_else(|| ConsoleAssets::get("index.html")) {
        Some(file) => (
            [(
                http::header::CONTENT_TYPE,
                file.metadata.mimetype().to_string(),
            )],
            axum::body::Body::from(file.data.into_owned()),
        )
            .into_response(),
        None => http::StatusCode::NOT_FOUND.into_response(),
    }
}

/// `dist/` produced by `vite build`, embedded behind the `console` feature.
#[cfg(feature = "console")]
#[derive(rust_embed::Embed)]
#[folder = "../../apps/console/dist"]
struct ConsoleAssets;

#[cfg(not(feature = "console"))]
fn serve_embedded_asset(_path: &str) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    (
        http::StatusCode::NOT_FOUND,
        "pawrly console assets are not bundled in this build (rebuild with \
         `--features console`), or point a standalone SPA at this gRPC-Web endpoint",
    )
        .into_response()
}
