//! Runtime store resolving dynamic source variables to current secrets.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use secrecy::{ExposeSecret as _, SecretString};
use tokio::sync::Mutex;

use pawrly_core::{DynamicVarSpec, TokenTransport, VarId};

pub mod discovery;
pub mod setup;
pub mod tokens;
pub use discovery::{ResolvedEndpoints, resolve as resolve_endpoints};
pub use setup::{
    DevicePrompt, authorization_code_connect, authorization_code_exchange, build_authorize_url,
    device_code_connect, pkce_pair,
};
pub use tokens::{
    EncryptedFileTokenStore, FileTokenStore, KeyringTokenStore, NoopTokenStore, TokenStoreError,
    VariableTokenStore, VariableValueStore, value_key,
};

const EXPIRY_SKEW: Duration = Duration::from_secs(30);
const DEFAULT_LIFETIME_SECS: u64 = 300;

#[derive(Debug, thiserror::Error)]
pub enum VarError {
    #[error("unknown variable id `{0}`")]
    Unknown(VarId),

    #[error("variable `{0}` is not connected; run source setup to authorize it")]
    NotConnected(VarId),

    #[error("token request failed: {0}")]
    Request(String),

    #[error("token endpoint returned HTTP {0}")]
    Status(u16),

    #[error("OAuth error from token endpoint (HTTP {status}): {error}")]
    OAuth { status: u16, error: String },

    #[error("token response invalid: {0}")]
    Response(String),

    #[error("device authorization flow: {0}")]
    DeviceFlow(String),

    #[error("persisting refresh token: {0}")]
    Persist(String),
}

/// Resolves dynamic variables to current values.
#[async_trait]
pub trait VariableStore: Send + Sync {
    async fn resolve(&self, id: &str) -> Result<SecretString, VarError>;
}

struct CachedToken {
    token: String,
    expires_at: SystemTime,
}

/// Locking the cell single-flights the mint, so concurrent resolves share one exchange.
type Cell = Arc<Mutex<Option<CachedToken>>>;

/// Variable store backed by live OAuth token exchanges.
pub struct RuntimeVariableStore {
    specs: HashMap<VarId, DynamicVarSpec>,
    cells: Mutex<HashMap<VarId, Cell>>,
    client: reqwest::Client,
    tokens: Arc<dyn VariableValueStore>,
    /// Where OIDC discovery documents are cached (`<dir>/oidc/`). `None` ⇒ no cache.
    cache_dir: Option<PathBuf>,
}

impl std::fmt::Debug for RuntimeVariableStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeVariableStore")
            .field("specs", &self.specs.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl RuntimeVariableStore {
    /// No token persistence.
    #[must_use]
    pub fn new(specs: HashMap<VarId, DynamicVarSpec>) -> Self {
        Self::build(specs, default_client(), Arc::new(NoopTokenStore))
    }

    #[must_use]
    pub fn with_client(specs: HashMap<VarId, DynamicVarSpec>, client: reqwest::Client) -> Self {
        Self::build(specs, client, Arc::new(NoopTokenStore))
    }

    /// With refresh-token persistence; required for interactive grants.
    #[must_use]
    pub fn with_tokens(
        specs: HashMap<VarId, DynamicVarSpec>,
        tokens: Arc<dyn VariableValueStore>,
    ) -> Self {
        Self::build(specs, default_client(), tokens)
    }

    fn build(
        specs: HashMap<VarId, DynamicVarSpec>,
        client: reqwest::Client,
        tokens: Arc<dyn VariableValueStore>,
    ) -> Self {
        Self {
            specs,
            cells: Mutex::new(HashMap::new()),
            client,
            tokens,
            cache_dir: None,
        }
    }

    /// Cache OIDC discovery documents under `dir/oidc/` (typically `<home>/cache`).
    #[must_use]
    pub fn with_cache_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.cache_dir = dir;
        self
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.specs.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    async fn cell(&self, id: &str) -> Cell {
        self.cells
            .lock()
            .await
            .entry(id.to_string())
            .or_default()
            .clone()
    }

    /// `client_credentials` mints directly; interactive grants refresh from the persisted token.
    async fn mint(&self, id: &str, spec: &DynamicVarSpec) -> Result<CachedToken, VarError> {
        let endpoints =
            discovery::resolve(spec.endpoints(), &self.client, self.cache_dir.as_deref()).await?;
        match spec {
            DynamicVarSpec::ClientCredentials { audience, .. } => {
                let mut extra: Vec<(&str, &str)> = vec![("grant_type", "client_credentials")];
                if let Some(scope) = spec.scope() {
                    extra.push(("scope", scope));
                }
                if let Some(a) = audience {
                    extra.push(("audience", a));
                }
                let resp = post_token(&self.client, spec, &endpoints.token_url, extra).await?;
                Ok(resp.into_cached())
            }
            DynamicVarSpec::DeviceCode { .. } | DynamicVarSpec::AuthorizationCode { .. } => {
                let stored = self
                    .tokens
                    .get(id)
                    .map_err(|e| VarError::Persist(e.to_string()))?
                    .ok_or_else(|| VarError::NotConnected(id.to_string()))?;
                let refresh = stored.expose_secret().to_string();
                let resp = post_token(
                    &self.client,
                    spec,
                    &endpoints.token_url,
                    vec![("grant_type", "refresh_token"), ("refresh_token", &refresh)],
                )
                .await?;
                if let Some(new_rt) = &resp.refresh_token
                    && *new_rt != refresh
                {
                    self.tokens
                        .set(id, &SecretString::from(new_rt.clone()))
                        .map_err(|e| VarError::Persist(e.to_string()))?;
                }
                Ok(resp.into_cached())
            }
        }
    }
}

/// POST a token request and parse a successful response.
pub(crate) async fn post_token(
    client: &reqwest::Client,
    spec: &DynamicVarSpec,
    token_url: &str,
    extra: Vec<(&str, &str)>,
) -> Result<TokenResponse, VarError> {
    let (status, body) = post_form(client, spec, token_url, extra).await?;
    if !status.is_success() {
        return Err(oauth_error(status.as_u16(), &body));
    }
    TokenResponse::parse(&body)
}

/// Token POST returning the raw status + JSON body, so the device-flow poll can read error bodies.
pub(crate) async fn post_form(
    client: &reqwest::Client,
    spec: &DynamicVarSpec,
    token_url: &str,
    extra: Vec<(&str, &str)>,
) -> Result<(reqwest::StatusCode, serde_json::Value), VarError> {
    let mut form = extra;
    let mut req = client
        .post(token_url)
        .header(reqwest::header::ACCEPT, "application/json");
    match spec.transport() {
        TokenTransport::BasicAuth => match spec.client_secret() {
            Some(secret) => req = req.basic_auth(spec.client_id(), Some(secret)),
            None => form.push(("client_id", spec.client_id())),
        },
        TokenTransport::RequestBody => {
            form.push(("client_id", spec.client_id()));
            if let Some(secret) = spec.client_secret() {
                form.push(("client_secret", secret));
            }
        }
    }
    let resp = req
        .form(&form)
        .send()
        .await
        .map_err(|e| VarError::Request(e.to_string()))?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    Ok((status, body))
}

pub(crate) struct TokenResponse {
    pub(crate) access_token: String,
    pub(crate) expires_in: u64,
    pub(crate) refresh_token: Option<String>,
}

impl TokenResponse {
    pub(crate) fn parse(body: &serde_json::Value) -> Result<Self, VarError> {
        let access_token = body
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| VarError::Response("missing `access_token`".to_string()))?
            .to_string();
        Ok(Self {
            access_token,
            expires_in: body
                .get("expires_in")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(DEFAULT_LIFETIME_SECS),
            refresh_token: body
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        })
    }

    fn into_cached(self) -> CachedToken {
        CachedToken {
            token: self.access_token,
            expires_at: SystemTime::now() + Duration::from_secs(self.expires_in),
        }
    }
}

fn oauth_error(status: u16, body: &serde_json::Value) -> VarError {
    match body.get("error").and_then(|v| v.as_str()) {
        Some(error) => VarError::OAuth {
            status,
            error: error.to_string(),
        },
        None => VarError::Status(status),
    }
}

#[async_trait]
impl VariableStore for RuntimeVariableStore {
    async fn resolve(&self, id: &str) -> Result<SecretString, VarError> {
        let spec = self
            .specs
            .get(id)
            .ok_or_else(|| VarError::Unknown(id.to_string()))?;

        let cell = self.cell(id).await;
        let mut guard = cell.lock().await;

        let fresh_until = SystemTime::now() + EXPIRY_SKEW;
        if let Some(cached) = guard.as_ref()
            && cached.expires_at > fresh_until
        {
            return Ok(SecretString::from(cached.token.clone()));
        }

        let minted = self.mint(id, spec).await?;
        let token = minted.token.clone();
        *guard = Some(minted);
        Ok(SecretString::from(token))
    }
}

fn default_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn cc(token_url: String, transport: TokenTransport) -> DynamicVarSpec {
        DynamicVarSpec::ClientCredentials {
            endpoints: pawrly_core::Endpoints::token(token_url),
            client_id: "id".to_string(),
            client_secret: "shh".to_string(),
            scope: None,
            audience: None,
            transport,
        }
    }

    fn store_with(id: &str, spec: DynamicVarSpec) -> RuntimeVariableStore {
        let mut specs = HashMap::new();
        specs.insert(id.to_string(), spec);
        RuntimeVariableStore::new(specs)
    }

    #[tokio::test]
    async fn client_credentials_request_body_mints_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=client_credentials"))
            .and(body_string_contains("client_secret=shh"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({ "access_token": "ya29.x", "expires_in": 3600 }),
                ),
            )
            .mount(&server)
            .await;

        let store = store_with(
            "s::T",
            cc(
                format!("{}/token", server.uri()),
                TokenTransport::RequestBody,
            ),
        );
        let tok = store.resolve("s::T").await.unwrap();
        assert_eq!(tok.expose_secret(), "ya29.x");
    }

    #[tokio::test]
    async fn resolves_token_endpoint_via_discovery() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token_endpoint": format!("{}/tok", server.uri())
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/tok"))
            .and(body_string_contains("grant_type=client_credentials"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "access_token": "at-disc", "expires_in": 3600 }),
            ))
            .mount(&server)
            .await;
        let spec = DynamicVarSpec::ClientCredentials {
            endpoints: pawrly_core::Endpoints {
                discovery: Some(format!("{}/.well-known/openid-configuration", server.uri())),
                ..Default::default()
            },
            client_id: "id".into(),
            client_secret: "shh".into(),
            scope: None,
            audience: None,
            transport: TokenTransport::RequestBody,
        };
        let tok = store_with("d::T", spec).resolve("d::T").await.unwrap();
        assert_eq!(tok.expose_secret(), "at-disc");
    }

    #[tokio::test]
    async fn basic_auth_transport_sends_authorization_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(wiremock::matchers::header(
                "authorization",
                "Basic aWQ6c2ho",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "access_token": "basic-ok", "expires_in": 3600 }),
            ))
            .mount(&server)
            .await;

        let store = store_with(
            "s::T",
            cc(format!("{}/token", server.uri()), TokenTransport::BasicAuth),
        );
        let tok = store.resolve("s::T").await.unwrap();
        assert_eq!(tok.expose_secret(), "basic-ok");
    }

    #[tokio::test]
    async fn caches_token_across_calls() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({ "access_token": "cached", "expires_in": 3600 }),
                ),
            )
            .expect(1)
            .mount(&server)
            .await;

        let store = store_with(
            "s::T",
            cc(
                format!("{}/token", server.uri()),
                TokenTransport::RequestBody,
            ),
        );
        assert_eq!(
            store.resolve("s::T").await.unwrap().expose_secret(),
            "cached"
        );
        assert_eq!(
            store.resolve("s::T").await.unwrap().expose_secret(),
            "cached"
        );
    }

    #[tokio::test]
    async fn re_mints_when_token_is_already_expired() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "access_token": "short", "expires_in": 1 })),
            )
            .expect(2)
            .mount(&server)
            .await;

        let store = store_with(
            "s::T",
            cc(
                format!("{}/token", server.uri()),
                TokenTransport::RequestBody,
            ),
        );
        store.resolve("s::T").await.unwrap();
        store.resolve("s::T").await.unwrap();
    }

    #[tokio::test]
    async fn single_flight_collapses_concurrent_resolves() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(50))
                    .set_body_json(
                        serde_json::json!({ "access_token": "one", "expires_in": 3600 }),
                    ),
            )
            .expect(1)
            .mount(&server)
            .await;

        let store = Arc::new(store_with(
            "s::T",
            cc(
                format!("{}/token", server.uri()),
                TokenTransport::RequestBody,
            ),
        ));
        let mut handles = Vec::new();
        for _ in 0..10 {
            let s = store.clone();
            handles.push(tokio::spawn(async move { s.resolve("s::T").await }));
        }
        for h in handles {
            assert_eq!(h.await.unwrap().unwrap().expose_secret(), "one");
        }
    }

    #[tokio::test]
    async fn unknown_id_errors() {
        let store = store_with(
            "s::T",
            cc(
                "http://unused/token".to_string(),
                TokenTransport::RequestBody,
            ),
        );
        let err = store.resolve("s::OTHER").await.unwrap_err();
        assert!(matches!(err, VarError::Unknown(id) if id == "s::OTHER"));
    }

    #[tokio::test]
    async fn http_error_surfaces_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let store = store_with(
            "s::T",
            cc(
                format!("{}/token", server.uri()),
                TokenTransport::RequestBody,
            ),
        );
        let err = store.resolve("s::T").await.unwrap_err();
        assert!(matches!(err, VarError::Status(401)));
    }

    #[tokio::test]
    async fn missing_access_token_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "nope": 1 })),
            )
            .mount(&server)
            .await;
        let store = store_with(
            "s::T",
            cc(
                format!("{}/token", server.uri()),
                TokenTransport::RequestBody,
            ),
        );
        let err = store.resolve("s::T").await.unwrap_err();
        assert!(matches!(err, VarError::Response(_)));
    }

    fn device_spec(token_url: String) -> DynamicVarSpec {
        DynamicVarSpec::DeviceCode {
            endpoints: pawrly_core::Endpoints {
                device_authorization_url: Some("https://idp/device".to_string()),
                token_url: Some(token_url),
                ..Default::default()
            },
            client_id: "cid".to_string(),
            client_secret: None,
            scope: None,
        }
    }

    #[tokio::test]
    async fn interactive_grant_refreshes_from_persisted_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("refresh_token=stored-rt"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "access_token": "fresh-at", "expires_in": 3600 }),
            ))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let tokens = Arc::new(FileTokenStore::new(dir.path().join("t.json")));
        tokens
            .set("gh::T", &SecretString::from("stored-rt".to_string()))
            .unwrap();

        let mut specs = HashMap::new();
        specs.insert(
            "gh::T".to_string(),
            device_spec(format!("{}/token", server.uri())),
        );
        let store = RuntimeVariableStore::with_tokens(specs, tokens);

        let tok = store.resolve("gh::T").await.unwrap();
        assert_eq!(tok.expose_secret(), "fresh-at");
    }

    #[tokio::test]
    async fn interactive_grant_rotates_refresh_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at", "refresh_token": "rotated-rt", "expires_in": 1
            })))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let tokens = Arc::new(FileTokenStore::new(dir.path().join("t.json")));
        tokens
            .set("gh::T", &SecretString::from("old-rt".to_string()))
            .unwrap();

        let mut specs = HashMap::new();
        specs.insert(
            "gh::T".to_string(),
            device_spec(format!("{}/token", server.uri())),
        );
        let store = RuntimeVariableStore::with_tokens(specs, tokens.clone());

        store.resolve("gh::T").await.unwrap();
        assert_eq!(
            tokens.get("gh::T").unwrap().unwrap().expose_secret(),
            "rotated-rt",
            "a returned refresh token should be persisted"
        );
    }

    #[tokio::test]
    async fn interactive_grant_not_connected_errors() {
        let mut specs = HashMap::new();
        specs.insert(
            "gh::T".to_string(),
            device_spec("https://idp/token".to_string()),
        );
        let store = RuntimeVariableStore::new(specs);
        let err = store.resolve("gh::T").await.unwrap_err();
        assert!(matches!(err, VarError::NotConnected(id) if id == "gh::T"));
    }
}
