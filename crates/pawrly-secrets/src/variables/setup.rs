//! Setup-time minting for the interactive grants.

use std::path::Path;
use std::time::{Duration, Instant};

use base64::Engine as _;
use rand::RngCore as _;
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use secrecy::SecretString;

use pawrly_core::DynamicVarSpec;

use super::{VarError, VariableValueStore, discovery, post_form, post_token};

/// RFC 8628
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const DEFAULT_INTERVAL_SECS: u64 = 5;
const DEFAULT_EXPIRES_SECS: u64 = 900;

/// What to show during a device-code flow.
#[derive(Debug, Clone)]
pub struct DevicePrompt {
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
}

/// Run the device-authorization flow and persist the refresh token.
pub async fn device_code_connect(
    spec: &DynamicVarSpec,
    client: &reqwest::Client,
    tokens: &dyn VariableValueStore,
    var_id: &str,
    cache_dir: Option<&Path>,
    on_prompt: impl FnOnce(&DevicePrompt),
) -> Result<(), VarError> {
    let DynamicVarSpec::DeviceCode { client_id, .. } = spec else {
        return Err(VarError::DeviceFlow(
            "device_code_connect called for a non-device_code variable".to_string(),
        ));
    };
    let endpoints = discovery::resolve(spec.endpoints(), client, cache_dir).await?;
    let device_authorization_url =
        endpoints
            .device_authorization_url
            .as_deref()
            .ok_or_else(|| {
                VarError::DeviceFlow(
                    "no device authorization endpoint (the discovery document has no \
             `device_authorization_endpoint`)"
                        .to_string(),
                )
            })?;

    let mut form: Vec<(&str, &str)> = vec![("client_id", client_id)];
    if let Some(scope) = spec.scope() {
        form.push(("scope", scope));
    }
    let resp = client
        .post(device_authorization_url)
        // Some providers (e.g. GitHub) return form-encoded unless JSON is asked for.
        .header(reqwest::header::ACCEPT, "application/json")
        .form(&form)
        .send()
        .await
        .map_err(|e| VarError::Request(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(VarError::DeviceFlow(format!(
            "device authorization returned HTTP {}",
            resp.status().as_u16()
        )));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| VarError::Response(format!("device authorization not JSON: {e}")))?;

    let device_code = str_field(&body, "device_code")?;
    let prompt = DevicePrompt {
        user_code: str_field(&body, "user_code")?,
        verification_uri: body
            .get("verification_uri")
            .or_else(|| body.get("verification_url"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| VarError::Response("missing `verification_uri`".to_string()))?
            .to_string(),
        verification_uri_complete: body
            .get("verification_uri_complete")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        expires_in: u64_field(&body, "expires_in", DEFAULT_EXPIRES_SECS),
    };
    let mut interval = Duration::from_secs(u64_field(&body, "interval", DEFAULT_INTERVAL_SECS));
    let deadline = Instant::now() + Duration::from_secs(prompt.expires_in);

    on_prompt(&prompt);

    loop {
        tokio::time::sleep(interval).await;
        if Instant::now() >= deadline {
            return Err(VarError::DeviceFlow(
                "device code expired before approval".to_string(),
            ));
        }
        let (status, body) = post_form(
            client,
            spec,
            &endpoints.token_url,
            vec![("grant_type", DEVICE_GRANT), ("device_code", &device_code)],
        )
        .await?;
        // Pending comes back as an error body with HTTP 200, so branch on the error before the status.
        match body.get("error").and_then(|v| v.as_str()) {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                interval += Duration::from_secs(5);
                continue;
            }
            Some(other) => {
                return Err(VarError::OAuth {
                    status: status.as_u16(),
                    error: other.to_string(),
                });
            }
            None => {}
        }
        if status.is_success() {
            let parsed = super::TokenResponse::parse(&body)?;
            let refresh = parsed.refresh_token.ok_or_else(|| {
                VarError::DeviceFlow("token response carried no `refresh_token`".to_string())
            })?;
            return tokens
                .set(var_id, &SecretString::from(refresh))
                .map_err(|e| VarError::Persist(e.to_string()));
        }
        return Err(VarError::Status(status.as_u16()));
    }
}

/// Exchange an authorization code for tokens and persist the refresh token.
#[allow(clippy::too_many_arguments)]
pub async fn authorization_code_exchange(
    spec: &DynamicVarSpec,
    client: &reqwest::Client,
    tokens: &dyn VariableValueStore,
    var_id: &str,
    code: &str,
    redirect_uri: &str,
    code_verifier: Option<&str>,
    token_url: &str,
) -> Result<(), VarError> {
    if !matches!(spec, DynamicVarSpec::AuthorizationCode { .. }) {
        return Err(VarError::DeviceFlow(
            "authorization_code_exchange called for a non-authorization_code variable".to_string(),
        ));
    }
    let mut extra: Vec<(&str, &str)> = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
    ];
    if let Some(verifier) = code_verifier {
        extra.push(("code_verifier", verifier));
    }
    let resp = post_token(client, spec, token_url, extra).await?;
    let refresh = resp.refresh_token.ok_or_else(|| {
        VarError::Response("authorization_code response carried no `refresh_token`".to_string())
    })?;
    tokens
        .set(var_id, &SecretString::from(refresh))
        .map_err(|e| VarError::Persist(e.to_string()))
}

/// Generate a PKCE verifier and its S256 challenge (RFC 7636).
#[must_use]
pub fn pkce_pair() -> (String, String) {
    let verifier = random_token(32);
    let digest = Sha256::digest(verifier.as_bytes());
    (verifier, b64url(digest.as_slice()))
}

fn random_token(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    b64url(&bytes)
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Build the authorization-request URL.
pub fn build_authorize_url(
    authorization_url: &str,
    client_id: &str,
    redirect_uri: &str,
    scope: Option<&str>,
    state: &str,
    code_challenge: Option<&str>,
) -> Result<String, VarError> {
    let mut url = url::Url::parse(authorization_url)
        .map_err(|e| VarError::DeviceFlow(format!("invalid authorization_url: {e}")))?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("response_type", "code");
        q.append_pair("client_id", client_id);
        q.append_pair("redirect_uri", redirect_uri);
        q.append_pair("state", state);
        if let Some(s) = scope {
            q.append_pair("scope", s);
        }
        if let Some(c) = code_challenge {
            q.append_pair("code_challenge", c);
            q.append_pair("code_challenge_method", "S256");
        }
    }
    Ok(url.to_string())
}

/// Run the authorization-code flow over a loopback callback and persist the refresh token.
pub async fn authorization_code_connect(
    spec: &DynamicVarSpec,
    client: &reqwest::Client,
    tokens: &dyn VariableValueStore,
    var_id: &str,
    cache_dir: Option<&Path>,
    on_url: impl FnOnce(&str),
) -> Result<(), VarError> {
    let DynamicVarSpec::AuthorizationCode {
        redirect_uri,
        port_mode,
        pkce,
        ..
    } = spec
    else {
        return Err(VarError::DeviceFlow(
            "authorization_code_connect called for a non-authorization_code variable".to_string(),
        ));
    };
    let endpoints = discovery::resolve(spec.endpoints(), client, cache_dir).await?;
    let authorization_url = endpoints.authorization_url.as_deref().ok_or_else(|| {
        VarError::DeviceFlow(
            "no authorization endpoint (the discovery document has no `authorization_endpoint`)"
                .to_string(),
        )
    })?;

    let parsed = url::Url::parse(redirect_uri)
        .map_err(|e| VarError::DeviceFlow(format!("invalid redirect_uri: {e}")))?;
    let path = match parsed.path() {
        "" => "/".to_string(),
        p => p.to_string(),
    };
    let bind_port = match port_mode {
        pawrly_core::PortMode::Fixed => parsed.port().unwrap_or(0),
        pawrly_core::PortMode::Random => 0,
    };
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", bind_port))
        .await
        .map_err(|e| VarError::DeviceFlow(format!("loopback bind failed: {e}")))?;
    let actual_port = listener
        .local_addr()
        .map_err(|e| VarError::DeviceFlow(e.to_string()))?
        .port();
    let effective_redirect = format!("http://127.0.0.1:{actual_port}{path}");

    let (verifier, challenge) = if *pkce {
        let (v, c) = pkce_pair();
        (Some(v), Some(c))
    } else {
        (None, None)
    };
    let state = random_token(16);
    let authorize_url = build_authorize_url(
        authorization_url,
        spec.client_id(),
        &effective_redirect,
        spec.scope(),
        &state,
        challenge.as_deref(),
    )?;

    on_url(&authorize_url);

    let (code, got_state) = accept_callback(&listener, &path).await?;
    if got_state != state {
        return Err(VarError::DeviceFlow(
            "OAuth state mismatch (possible CSRF) — aborting".to_string(),
        ));
    }
    authorization_code_exchange(
        spec,
        client,
        tokens,
        var_id,
        &code,
        &effective_redirect,
        verifier.as_deref(),
        &endpoints.token_url,
    )
    .await
}

async fn accept_callback(
    listener: &tokio::net::TcpListener,
    expected_path: &str,
) -> Result<(String, String), VarError> {
    loop {
        let (mut stream, _) = listener
            .accept()
            .await
            .map_err(|e| VarError::DeviceFlow(format!("loopback accept: {e}")))?;
        let mut buf = [0u8; 8192];
        let n = stream
            .read(&mut buf)
            .await
            .map_err(|e| VarError::DeviceFlow(e.to_string()))?;
        let req = String::from_utf8_lossy(&buf[..n]);
        let target = req
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("");
        let (req_path, query) = target.split_once('?').unwrap_or((target, ""));
        if req_path != expected_path {
            respond(&mut stream, "404 Not Found", "Not found.").await;
            continue;
        }
        let params: std::collections::HashMap<String, String> =
            url::form_urlencoded::parse(query.as_bytes())
                .into_owned()
                .collect();
        if let Some(err) = params.get("error") {
            respond(
                &mut stream,
                "200 OK",
                "Authorization failed. You can close this tab.",
            )
            .await;
            return Err(VarError::OAuth {
                status: 0,
                error: err.clone(),
            });
        }
        let Some(code) = params.get("code") else {
            respond(
                &mut stream,
                "400 Bad Request",
                "Missing authorization code.",
            )
            .await;
            continue;
        };
        let state = params.get("state").cloned().unwrap_or_default();
        write_response(&mut stream, "200 OK", &page(CONNECTED_INNER)).await;
        return Ok((code.clone(), state));
    }
}

const CONNECTED_INNER: &str = "\
<div style=\"display:flex;align-items:center;justify-content:center;gap:.5rem;font-size:1.5rem;font-weight:600\">\
<svg width=\"28\" height=\"28\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"#16a34a\" stroke-width=\"3\" \
stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\"><polyline points=\"20 6 9 17 4 12\"/></svg>\
<span>Connected</span></div>\
<p style=\"margin:.75rem 0 0;color:#555\">You can close this tab and return to the terminal.</p>";

fn page(inner: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>Pawrly</title></head>\
         <body style=\"margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;\
         font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;\
         background:#fafafa;color:#111\">\
         <div style=\"text-align:center;padding:2rem\">{inner}</div></body></html>"
    )
}

async fn respond(stream: &mut tokio::net::TcpStream, status: &str, message: &str) {
    let inner = format!("<p style=\"margin:0;font-size:1.05rem\">{message}</p>");
    write_response(stream, status, &page(&inner)).await;
}

async fn write_response(stream: &mut tokio::net::TcpStream, status: &str, body: &str) {
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.flush().await;
}

fn str_field(body: &serde_json::Value, key: &str) -> Result<String, VarError> {
    body.get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| VarError::Response(format!("missing `{key}`")))
}

fn u64_field(body: &serde_json::Value, key: &str, default: u64) -> u64 {
    body.get(key)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(default)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use crate::FileTokenStore;
    use secrecy::ExposeSecret as _;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn device_spec(base: &str) -> DynamicVarSpec {
        DynamicVarSpec::DeviceCode {
            endpoints: pawrly_core::Endpoints {
                device_authorization_url: Some(format!("{base}/device/code")),
                token_url: Some(format!("{base}/token")),
                ..Default::default()
            },
            client_id: "cid".into(),
            client_secret: None,
            scope: Some("repo".into()),
        }
    }

    fn authz_spec(base: &str) -> DynamicVarSpec {
        DynamicVarSpec::AuthorizationCode {
            endpoints: pawrly_core::Endpoints {
                authorization_url: Some(format!("{base}/authorize")),
                token_url: Some(format!("{base}/token")),
                ..Default::default()
            },
            client_id: "cid".into(),
            client_secret: Some("csec".into()),
            redirect_uri: "http://127.0.0.1:0/callback".into(),
            port_mode: pawrly_core::PortMode::Random,
            pkce: true,
            scope: None,
            transport: pawrly_core::TokenTransport::RequestBody,
        }
    }

    #[tokio::test]
    async fn device_flow_polls_then_persists_refresh() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/device/code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "dev-123",
                "user_code": "WXYZ-1234",
                "verification_uri": "https://example.com/device",
                "interval": 0,
                "expires_in": 60
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=urn"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "error": "authorization_pending" })),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=urn"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-1", "refresh_token": "rt-1", "expires_in": 3600
            })))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let tokens = FileTokenStore::new(dir.path().join("t.json"));
        let mut prompted = None;
        device_code_connect(
            &device_spec(&server.uri()),
            &reqwest::Client::new(),
            &tokens,
            "gh::T",
            None,
            |p| prompted = Some(p.user_code.clone()),
        )
        .await
        .expect("device flow");

        assert_eq!(prompted.as_deref(), Some("WXYZ-1234"));
        assert_eq!(
            tokens.get("gh::T").unwrap().unwrap().expose_secret(),
            "rt-1"
        );
    }

    #[test]
    fn pkce_pair_matches_s256() {
        let (verifier, challenge) = pkce_pair();
        assert_eq!(verifier.len(), 43, "32 bytes base64url = 43 chars");
        let expected = b64url(Sha256::digest(verifier.as_bytes()).as_slice());
        assert_eq!(challenge, expected);
        assert!(!challenge.contains('='), "no padding");
        assert!(
            !challenge.contains('+') && !challenge.contains('/'),
            "url-safe alphabet"
        );
    }

    #[test]
    fn authorize_url_carries_pkce_and_state() {
        let url = build_authorize_url(
            "https://idp/authorize",
            "cid",
            "http://127.0.0.1:5000/callback",
            Some("read write"),
            "the-state",
            Some("the-challenge"),
        )
        .unwrap();
        let parsed = url::Url::parse(&url).unwrap();
        let q: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["client_id"], "cid");
        assert_eq!(q["redirect_uri"], "http://127.0.0.1:5000/callback");
        assert_eq!(q["state"], "the-state");
        assert_eq!(q["scope"], "read write");
        assert_eq!(q["code_challenge"], "the-challenge");
        assert_eq!(q["code_challenge_method"], "S256");
    }

    #[tokio::test]
    async fn authorization_code_loopback_flow_persists_refresh() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code=the-code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at", "refresh_token": "rt-loop", "expires_in": 3600
            })))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let tokens = FileTokenStore::new(dir.path().join("t.json"));

        authorization_code_connect(
            &authz_spec(&server.uri()),
            &reqwest::Client::new(),
            &tokens,
            "sf::T",
            None,
            |authorize_url| {
                let parsed = url::Url::parse(authorize_url).unwrap();
                let q: std::collections::HashMap<_, _> =
                    parsed.query_pairs().into_owned().collect();
                let callback = format!("{}?code=the-code&state={}", q["redirect_uri"], q["state"]);
                tokio::spawn(async move {
                    let _ = reqwest::get(&callback).await;
                });
            },
        )
        .await
        .expect("loopback flow");

        assert_eq!(
            tokens.get("sf::T").unwrap().unwrap().expose_secret(),
            "rt-loop"
        );
    }

    #[tokio::test]
    async fn authorization_code_exchange_persists_refresh() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code_verifier=verifier-xyz"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at", "refresh_token": "rt-ac", "expires_in": 3600
            })))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let tokens = FileTokenStore::new(dir.path().join("t.json"));
        authorization_code_exchange(
            &authz_spec(&server.uri()),
            &reqwest::Client::new(),
            &tokens,
            "sf::T",
            "the-code",
            "http://127.0.0.1:54321/callback",
            Some("verifier-xyz"),
            &format!("{}/token", server.uri()),
        )
        .await
        .expect("exchange");
        assert_eq!(
            tokens.get("sf::T").unwrap().unwrap().expose_secret(),
            "rt-ac"
        );
    }
}
