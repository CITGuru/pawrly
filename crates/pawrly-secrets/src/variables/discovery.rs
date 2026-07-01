//! OIDC discovery: turn an `Endpoints` (explicit and/or a `.well-known/
//! openid-configuration` URL) into concrete endpoints at first use. The fetched
//! document is cached on disk (`<cache>/oidc/<hash>.json`) with a TTL; an
//! explicit endpoint always overrides the discovered one.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use sha2::{Digest as _, Sha256};

use pawrly_core::Endpoints;

use super::VarError;

/// How long a cached discovery document is trusted before re-fetching.
const TTL_SECS: u64 = 24 * 60 * 60;

/// Concrete endpoints for a grant, after any discovery fetch + override merge.
#[derive(Debug, Clone)]
pub struct ResolvedEndpoints {
    pub authorization_url: Option<String>,
    pub device_authorization_url: Option<String>,
    pub token_url: String,
}

/// Resolve `endpoints` to concrete URLs, fetching the discovery document (cached
/// under `cache_dir`, when given) only if a URL is missing.
pub async fn resolve(
    endpoints: &Endpoints,
    client: &reqwest::Client,
    cache_dir: Option<&Path>,
) -> Result<ResolvedEndpoints, VarError> {
    let mut authorization_url = endpoints.authorization_url.clone();
    let mut device_authorization_url = endpoints.device_authorization_url.clone();
    let mut token_url = endpoints.token_url.clone();

    if let Some(discovery) = &endpoints.discovery {
        let doc = fetch(discovery, client, cache_dir).await?;
        let from = |key: &str| doc.get(key).and_then(|v| v.as_str()).map(str::to_string);
        // Explicit values win; discovery fills only what is absent.
        authorization_url = authorization_url.or_else(|| from("authorization_endpoint"));
        device_authorization_url =
            device_authorization_url.or_else(|| from("device_authorization_endpoint"));
        token_url = token_url.or_else(|| from("token_endpoint"));
    }

    let token_url = token_url.ok_or_else(|| {
        VarError::Response(
            "no token endpoint (discovery document has no `token_endpoint` and no explicit \
             `token_url` was given)"
                .to_string(),
        )
    })?;
    Ok(ResolvedEndpoints {
        authorization_url,
        device_authorization_url,
        token_url,
    })
}

/// Fetch a discovery document, preferring a fresh on-disk cache entry.
async fn fetch(
    url: &str,
    client: &reqwest::Client,
    cache_dir: Option<&Path>,
) -> Result<serde_json::Value, VarError> {
    if let Some(dir) = cache_dir
        && let Some(doc) = read_cache(dir, url)
    {
        return Ok(doc);
    }
    if !url_allowed(url) {
        return Err(VarError::Response(
            "`endpoints.discovery` must be https (or http to a loopback host)".to_string(),
        ));
    }
    let resp = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| VarError::Request(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(VarError::Response(format!(
            "discovery `{url}` returned HTTP {}",
            resp.status().as_u16()
        )));
    }
    let doc: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| VarError::Response(format!("discovery document is not JSON: {e}")))?;
    if let Some(dir) = cache_dir {
        write_cache(dir, url, &doc);
    }
    Ok(doc)
}

/// A discovery URL is https, or http to a loopback host (a local dev IdP).
pub fn url_allowed(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else {
        return false;
    };
    match u.scheme() {
        "https" => true,
        "http" => match u.host() {
            Some(url::Host::Domain(h)) => h.eq_ignore_ascii_case("localhost"),
            Some(url::Host::Ipv4(a)) => a.is_loopback(),
            Some(url::Host::Ipv6(a)) => a.is_loopback(),
            None => false,
        },
        _ => false,
    }
}

fn cache_path(dir: &Path, url: &str) -> PathBuf {
    let hash = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(url));
    dir.join("oidc").join(format!("{hash}.json"))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A fresh cached document, or `None` if absent, unreadable, or stale.
fn read_cache(dir: &Path, url: &str) -> Option<serde_json::Value> {
    let raw = std::fs::read_to_string(cache_path(dir, url)).ok()?;
    let wrapped: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let fetched_at = wrapped.get("fetched_at")?.as_u64()?;
    (now_secs().saturating_sub(fetched_at) <= TTL_SECS).then(|| wrapped.get("doc").cloned())?
}

fn write_cache(dir: &Path, url: &str, doc: &serde_json::Value) {
    let path = cache_path(dir, url);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let wrapped = serde_json::json!({ "fetched_at": now_secs(), "doc": doc });
    if let Ok(body) = serde_json::to_string(&wrapped) {
        let _ = std::fs::write(&path, body);
    }
}
