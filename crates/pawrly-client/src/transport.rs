//! Transport selection for connecting to a Pawrly server.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use tonic::transport::{Certificate, Channel, ClientTlsConfig, Identity};

/// Failure opening a [`Channel`] — a transport error, or an unreadable TLS file.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error(transparent)]
    Transport(#[from] tonic::transport::Error),
    #[error("reading TLS file `{path}`: {source}")]
    TlsFile {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("endpoint is not a gRPC transport (use `pawrly_client::connect`)")]
    NotGrpc,
}

/// How the client should reach the server.
#[derive(Debug, Clone)]
pub enum Endpoint {
    /// Plain TCP. Bearer token + optional TLS via [`TlsConfig`].
    Tcp {
        addr: SocketAddr,
        bearer: Option<String>,
        tls: Option<TlsConfig>,
    },
    /// Unix domain socket. POSIX file perms are the trust boundary.
    #[cfg(unix)]
    Uds { path: PathBuf },
    /// A pre-existing tonic channel; used for tests.
    InProcess(Channel),
    /// REST/JSON over HTTP (`pawrly console` / `serve --console`). `base_url`
    /// like `http://127.0.0.1:8787`; bearer carried per request.
    Rest {
        base_url: String,
        bearer: Option<String>,
    },
}

#[derive(Debug, Clone, Default)]
pub struct TlsConfig {
    pub ca_cert: Option<PathBuf>,
    pub client_cert: Option<PathBuf>,
    pub client_key: Option<PathBuf>,
    pub domain_name: Option<String>,
}

impl Endpoint {
    /// The bearer token configured for this endpoint, if any. Carried at the
    /// application layer (an `authorization` header injected per request), not
    /// by the transport's `connect`.
    #[must_use]
    pub fn bearer_token(&self) -> Option<String> {
        match self {
            Endpoint::Tcp { bearer, .. } | Endpoint::Rest { bearer, .. } => bearer.clone(),
            _ => None,
        }
    }

    /// Open a tonic Channel for this endpoint. The bearer token is carried at
    /// the application layer (see [`Self::bearer_token`]); TLS is configured
    /// here when [`TlsConfig`] is present.
    pub async fn connect(self) -> Result<Channel, ConnectError> {
        match self {
            Endpoint::Tcp {
                addr,
                bearer: _,
                tls,
            } => {
                let scheme = if tls.is_some() { "https" } else { "http" };
                let mut endpoint =
                    tonic::transport::Endpoint::try_from(format!("{scheme}://{addr}"))?;
                if let Some(tls) = tls {
                    endpoint = endpoint.tls_config(build_client_tls(&tls)?)?;
                }
                Ok(endpoint.connect().await?)
            }
            #[cfg(unix)]
            Endpoint::Uds { path } => {
                let path_clone = path.clone();
                let endpoint = tonic::transport::Endpoint::try_from("http://[::]:50051")?;
                let channel = endpoint
                    .connect_with_connector(tower::service_fn(move |_| {
                        let p = path_clone.clone();
                        async move {
                            let s = tokio::net::UnixStream::connect(&p).await?;
                            Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(s))
                        }
                    }))
                    .await?;
                Ok(channel)
            }
            Endpoint::InProcess(channel) => Ok(channel),
            Endpoint::Rest { .. } => Err(ConnectError::NotGrpc),
        }
    }
}

/// Read a PEM file, tagging IO errors with the path for a clear message.
fn read_pem(path: &Path) -> Result<Vec<u8>, ConnectError> {
    std::fs::read(path).map_err(|source| ConnectError::TlsFile {
        path: path.display().to_string(),
        source,
    })
}

/// Build a [`ClientTlsConfig`] from a [`TlsConfig`]: a custom CA (else the
/// platform's native roots), an optional client identity for mTLS, and an
/// optional server-name override (e.g. connecting to an IP while the cert is
/// issued for a hostname).
fn build_client_tls(tls: &TlsConfig) -> Result<ClientTlsConfig, ConnectError> {
    // With a custom CA, trust exactly that; otherwise fall back to whatever
    // root certificates are compiled into tonic (enable a `tls-*-roots` feature
    // for public-CA verification).
    let mut cfg = match &tls.ca_cert {
        Some(ca) => ClientTlsConfig::new().ca_certificate(Certificate::from_pem(read_pem(ca)?)),
        None => ClientTlsConfig::new().with_enabled_roots(),
    };
    if let (Some(cert), Some(key)) = (&tls.client_cert, &tls.client_key) {
        cfg = cfg.identity(Identity::from_pem(read_pem(cert)?, read_pem(key)?));
    }
    if let Some(domain) = &tls.domain_name {
        cfg = cfg.domain_name(domain.clone());
    }
    Ok(cfg)
}
