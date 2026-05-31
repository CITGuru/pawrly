//! Bearer-token authentication for the gRPC server.
//!
//! [`AuthInterceptor`] is a tonic interceptor applied to every service. For
//! [`AuthMode::Bearer`], a request must carry `authorization: Bearer <token>`
//! matching the configured token (compared in constant time) or it is rejected
//! with `Unauthenticated`. For [`AuthMode::None`], every request passes — that
//! mode is only reachable on UDS or loopback TCP (see `serve_tcp`).

use std::sync::Arc;

use tonic::service::Interceptor;
use tonic::{Request, Status};

/// Configured authentication mode.
#[derive(Clone, Default)]
pub enum AuthMode {
    /// No authentication. Allowed on UDS or loopback TCP only.
    #[default]
    None,
    /// Bearer token. Required for non-loopback TCP.
    Bearer { token: String },
}

/// Enforces [`AuthMode`] on every incoming request.
#[derive(Clone, Default)]
pub struct AuthInterceptor {
    /// `None` when auth is disabled; otherwise the expected token.
    expected: Option<Arc<str>>,
}

impl AuthInterceptor {
    /// Build an interceptor for the given mode.
    #[must_use]
    pub fn new(mode: &AuthMode) -> Self {
        let expected = match mode {
            AuthMode::None => None,
            AuthMode::Bearer { token } => Some(Arc::from(token.as_str())),
        };
        Self { expected }
    }
}

impl Interceptor for AuthInterceptor {
    fn call(&mut self, req: Request<()>) -> Result<Request<()>, Status> {
        let Some(expected) = &self.expected else {
            return Ok(req);
        };
        let presented = req
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(bearer_token);
        match presented {
            Some(tok) if constant_time_eq(tok.as_bytes(), expected.as_bytes()) => Ok(req),
            _ => Err(Status::unauthenticated("missing or invalid bearer token")),
        }
    }
}

/// Extract the token from an `Authorization` header value, matching the
/// `Bearer` scheme case-insensitively.
fn bearer_token(header: &str) -> Option<&str> {
    let (scheme, token) = header.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim())
        .filter(|t| !t.is_empty())
}

/// Constant-time byte equality (over equal-length inputs). Length mismatch
/// returns early — a token's length is not the secret.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req_with(header: Option<&str>) -> Request<()> {
        let mut req = Request::new(());
        if let Some(h) = header {
            req.metadata_mut()
                .insert("authorization", h.parse().unwrap());
        }
        req
    }

    #[test]
    fn none_mode_passes_everything() {
        let mut i = AuthInterceptor::new(&AuthMode::None);
        assert!(i.call(req_with(None)).is_ok());
    }

    #[test]
    fn bearer_requires_matching_token() {
        let mut i = AuthInterceptor::new(&AuthMode::Bearer {
            token: "s3cret".into(),
        });
        assert!(i.call(req_with(Some("Bearer s3cret"))).is_ok());
        assert!(i.call(req_with(Some("bearer s3cret"))).is_ok()); // scheme is case-insensitive
        assert!(i.call(req_with(Some("Bearer wrong"))).is_err());
        assert!(i.call(req_with(Some("s3cret"))).is_err()); // no scheme
        assert!(i.call(req_with(None)).is_err());
    }
}
