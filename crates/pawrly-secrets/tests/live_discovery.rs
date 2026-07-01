//! Live OIDC discovery against a real provider (Spotify). Ignored by default:
//! both tests hit the network, and the mint test needs real client-credentials
//! creds in `SPOTIFY_CLIENT_ID` / `SPOTIFY_CLIENT_SECRET`.
//!
//! Run:
//!   cargo test -p pawrly-secrets --test live_discovery -- --ignored --nocapture

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::collections::HashMap;

use pawrly_core::{DynamicVarSpec, Endpoints, TokenTransport};
use pawrly_secrets::variables::resolve_endpoints;
use pawrly_secrets::{RuntimeVariableStore, VariableStore};
use secrecy::ExposeSecret as _;

const SPOTIFY_DISCOVERY: &str = "https://accounts.spotify.com/.well-known/openid-configuration";

/// Discovery alone: GET the real document and confirm we extract Spotify's
/// published `token_endpoint` / `authorization_endpoint`. No creds required.
#[tokio::test]
#[ignore = "live network: GETs accounts.spotify.com discovery document"]
async fn spotify_discovery_resolves_real_endpoints() {
    let endpoints = Endpoints {
        discovery: Some(SPOTIFY_DISCOVERY.to_string()),
        ..Default::default()
    };
    let resolved = resolve_endpoints(&endpoints, &reqwest::Client::new(), None)
        .await
        .expect("discovery should resolve against live Spotify");

    assert_eq!(resolved.token_url, "https://accounts.spotify.com/api/token");
    assert_eq!(
        resolved.authorization_url.as_deref(),
        Some("https://accounts.spotify.com/oauth2/v2/auth")
    );
    eprintln!("discovery -> token_url = {}", resolved.token_url);
}

/// Full chain: live discovery -> discovered `token_endpoint` -> real
/// client-credentials mint, exactly as `mint()` runs it in production.
#[tokio::test]
#[ignore = "live network + creds: SPOTIFY_CLIENT_ID / SPOTIFY_CLIENT_SECRET"]
async fn spotify_client_credentials_mints_via_discovery() {
    let client_id = std::env::var("SPOTIFY_CLIENT_ID")
        .expect("set SPOTIFY_CLIENT_ID to run the live mint test");
    let client_secret = std::env::var("SPOTIFY_CLIENT_SECRET")
        .expect("set SPOTIFY_CLIENT_SECRET to run the live mint test");

    let mut specs = HashMap::new();
    specs.insert(
        "spotify::TOKEN".to_string(),
        DynamicVarSpec::ClientCredentials {
            endpoints: Endpoints {
                discovery: Some(SPOTIFY_DISCOVERY.to_string()),
                ..Default::default()
            },
            client_id,
            client_secret,
            scope: None,
            audience: None,
            transport: TokenTransport::RequestBody,
        },
    );

    let token = RuntimeVariableStore::new(specs)
        .resolve("spotify::TOKEN")
        .await
        .expect("mint a real token through the discovered endpoint");

    assert!(!token.expose_secret().is_empty());
    eprintln!("minted Spotify token (len {})", token.expose_secret().len());
}
