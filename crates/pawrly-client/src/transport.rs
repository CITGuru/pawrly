//! Transport selection for connecting to a Pawrly server.

use std::net::SocketAddr;
use std::path::PathBuf;

use tonic::transport::Channel;

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
}

#[derive(Debug, Clone, Default)]
pub struct TlsConfig {
    pub ca_cert: Option<PathBuf>,
    pub client_cert: Option<PathBuf>,
    pub client_key: Option<PathBuf>,
    pub domain_name: Option<String>,
}

impl Endpoint {
    /// Open a tonic Channel for this endpoint.
    pub async fn connect(self) -> Result<Channel, tonic::transport::Error> {
        match self {
            Endpoint::Tcp {
                addr,
                bearer: _,
                tls: _,
            } => {
                // bearer + TLS plumbing not yet implemented.
                let uri = format!("http://{addr}");
                tonic::transport::Endpoint::try_from(uri)?.connect().await
            }
            #[cfg(unix)]
            Endpoint::Uds { path } => {
                let path_clone = path.clone();
                let endpoint = tonic::transport::Endpoint::try_from("http://[::]:50051")?;
                endpoint
                    .connect_with_connector(tower::service_fn(move |_| {
                        let p = path_clone.clone();
                        async move {
                            let s = tokio::net::UnixStream::connect(&p).await?;
                            Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(s))
                        }
                    }))
                    .await
            }
            Endpoint::InProcess(channel) => Ok(channel),
        }
    }
}
