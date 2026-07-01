//! Engine-facing descriptors for *dynamic* (OAuth-minted) source variables.
//! Static variables never appear here — they are inlined into `SourceDef.config`
//! at load.

use serde::{Deserialize, Serialize};

/// Scope-unique key for one dynamic variable: `"{declaring_scope}::{NAME}"`.
/// Sources sharing a declaration share a `VarId` (one minted identity).
pub type VarId = String;

/// One dynamic `${var:NAME}` reference resolved at load.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DynamicVarBinding {
    pub name: String,
    pub id: VarId,
    pub spec: DynamicVarSpec,
}

/// A grant's OAuth endpoints: either given explicitly, or read from an OIDC
/// discovery document (`discovery` = a `.well-known/openid-configuration` URL)
/// fetched at first use. An explicit field overrides the discovered one.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Endpoints {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_authorization_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_url: Option<String>,
}

impl Endpoints {
    /// Explicit endpoints with just a token URL (the rest set per grant).
    #[must_use]
    pub fn token(token_url: impl Into<String>) -> Self {
        Self {
            token_url: Some(token_url.into()),
            ..Self::default()
        }
    }
}

/// How a dynamic variable's value is minted. `client_credentials` mints lazily at
/// runtime; the interactive grants mint at setup and refresh at runtime from a
/// persisted refresh token.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "grant", rename_all = "snake_case")]
pub enum DynamicVarSpec {
    ClientCredentials {
        endpoints: Endpoints,
        client_id: String,
        client_secret: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audience: Option<String>,
        #[serde(default)]
        transport: TokenTransport,
    },
    /// OAuth 2.0 device-authorization grant (RFC 8628) — no callback, works headless.
    DeviceCode {
        endpoints: Endpoints,
        client_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_secret: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
    },
    /// OAuth 2.0 authorization-code grant (loopback callback, optional PKCE).
    AuthorizationCode {
        endpoints: Endpoints,
        client_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_secret: Option<String>,
        redirect_uri: String,
        #[serde(default)]
        port_mode: PortMode,
        #[serde(default)]
        pkce: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
        #[serde(default)]
        transport: TokenTransport,
    },
}

/// Loopback redirect port selection for `authorization_code`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortMode {
    /// Bind any free port (RFC 8252). The default.
    #[default]
    Random,
    /// Bind the exact port in `redirect_uri`.
    Fixed,
}

impl DynamicVarSpec {
    #[must_use]
    pub fn endpoints(&self) -> &Endpoints {
        match self {
            Self::ClientCredentials { endpoints, .. }
            | Self::DeviceCode { endpoints, .. }
            | Self::AuthorizationCode { endpoints, .. } => endpoints,
        }
    }

    #[must_use]
    pub fn client_id(&self) -> &str {
        match self {
            Self::ClientCredentials { client_id, .. }
            | Self::DeviceCode { client_id, .. }
            | Self::AuthorizationCode { client_id, .. } => client_id,
        }
    }

    #[must_use]
    pub fn client_secret(&self) -> Option<&str> {
        match self {
            Self::ClientCredentials { client_secret, .. } => Some(client_secret),
            Self::DeviceCode { client_secret, .. }
            | Self::AuthorizationCode { client_secret, .. } => client_secret.as_deref(),
        }
    }

    #[must_use]
    pub fn transport(&self) -> TokenTransport {
        match self {
            Self::ClientCredentials { transport, .. }
            | Self::AuthorizationCode { transport, .. } => *transport,
            Self::DeviceCode { .. } => TokenTransport::RequestBody,
        }
    }

    #[must_use]
    pub fn scope(&self) -> Option<&str> {
        match self {
            Self::ClientCredentials { scope, .. }
            | Self::DeviceCode { scope, .. }
            | Self::AuthorizationCode { scope, .. } => scope.as_deref(),
        }
    }

    #[must_use]
    pub fn is_interactive(&self) -> bool {
        matches!(
            self,
            Self::DeviceCode { .. } | Self::AuthorizationCode { .. }
        )
    }

    #[must_use]
    pub fn grant(&self) -> &'static str {
        match self {
            Self::ClientCredentials { .. } => "client_credentials",
            Self::DeviceCode { .. } => "device_code",
            Self::AuthorizationCode { .. } => "authorization_code",
        }
    }
}

/// How OAuth client credentials are presented to the token endpoint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenTransport {
    /// `client_id` / `client_secret` in the form body (the default).
    #[default]
    RequestBody,
    /// HTTP Basic auth header carrying the client id/secret.
    BasicAuth,
}
