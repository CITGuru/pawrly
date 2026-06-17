//! Streamable HTTP transport for MCP. A single `/mcp` endpoint accepts a
//! JSON-RPC message per POST and replies with a JSON-RPC response (or `202` for
//! notifications). `/healthz` is an unauthenticated liveness probe.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use opentelemetry::propagation::Extractor;
use pawrly_core::EngineService;
use serde_json::Value;
use subtle::ConstantTimeEq as _;
use tracing::Instrument as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

use crate::cancel::CancelRegistry;
use crate::dispatch::{error_response, handle_message};

/// Options for the HTTP transport.
pub struct HttpOpts {
    pub addr: SocketAddr,
    /// When set, every `/mcp` request must present `Authorization: Bearer
    /// <token>`. When unset, the server only binds a loopback address.
    pub bearer_token: Option<String>,
}

struct AppState {
    engine: Arc<dyn EngineService>,
    bearer_token: Option<String>,
    cancel: CancelRegistry,
}

/// Serve MCP over HTTP until the process is terminated.
pub async fn serve_http(engine: Arc<dyn EngineService>, opts: HttpOpts) -> std::io::Result<()> {
    if opts.bearer_token.is_none() && !opts.addr.ip().is_loopback() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing to bind a non-loopback address without --bearer-token-from",
        ));
    }
    let state = Arc::new(AppState {
        engine,
        bearer_token: opts.bearer_token,
        cancel: CancelRegistry::new(),
    });
    let listener = tokio::net::TcpListener::bind(opts.addr).await?;
    tracing::info!(addr = %opts.addr, "starting pawrly MCP server (HTTP)");
    axum::serve(listener, router(state)).await
}

fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/mcp", post(mcp_post).get(mcp_get))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(state)
}

fn authorized(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(expected) = &state.bearer_token else {
        return true;
    };
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| {
            // Constant-time comparison to avoid leaking the token via timing.
            t.as_bytes().ct_eq(expected.as_bytes()).into()
        })
}

async fn mcp_post(State(state): State<Arc<AppState>>, headers: HeaderMap, body: Bytes) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return Json(error_response(
                Value::Null,
                -32700,
                &format!("parse error: {e}"),
            ))
            .into_response();
        }
    };
    // Root the request span at the caller's trace context when propagated, so a
    // remote client and this server share one trace. No-op when OTel is off.
    let span = tracing::info_span!("pawrly.mcp.request", pawrly.interface = "mcp");
    let _ = span.set_parent(
        opentelemetry::global::get_text_map_propagator(|p| p.extract(&HeaderExtractor(&headers))),
    );
    match handle_message(&state.engine, &state.cancel, &req)
        .instrument(span)
        .await
    {
        Some(resp) => Json(resp).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// Read-only view of request headers for the W3C propagator.
struct HeaderExtractor<'a>(&'a HeaderMap);

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
    }
}

async fn mcp_get(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, "missing or invalid bearer token").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use pawrly_core::test_support::MockEngine;
    use serde_json::json;
    use tower::ServiceExt;

    fn state(bearer_token: Option<String>) -> Arc<AppState> {
        Arc::new(AppState {
            engine: Arc::new(MockEngine::new()),
            bearer_token,
            cancel: CancelRegistry::new(),
        })
    }

    fn post(body: &Value, auth: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json");
        if let Some(token) = auth {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        builder
            .body(Body::from(serde_json::to_vec(body).unwrap()))
            .unwrap()
    }

    async fn body_json(resp: Response) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn healthz_is_ok() {
        let req = Request::builder()
            .uri("/healthz")
            .body(Body::empty())
            .unwrap();
        let resp = router(state(None)).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn initialize_round_trips() {
        let req = post(
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }),
            None,
        );
        let resp = router(state(None)).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert!(v["result"]["serverInfo"].is_object());
    }

    #[tokio::test]
    async fn notification_returns_202() {
        let req = post(
            &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            None,
        );
        let resp = router(state(None)).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn get_mcp_is_method_not_allowed() {
        let req = Request::builder().uri("/mcp").body(Body::empty()).unwrap();
        let resp = router(state(None)).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn parse_error_is_reported() {
        let req = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .body(Body::from("not json"))
            .unwrap();
        let resp = router(state(None)).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], -32700);
    }

    #[tokio::test]
    async fn missing_token_is_rejected() {
        let req = post(
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }),
            None,
        );
        let resp = router(state(Some("secret".into())))
            .oneshot(req)
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn wrong_token_is_rejected() {
        let req = post(
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }),
            Some("nope"),
        );
        let resp = router(state(Some("secret".into())))
            .oneshot(req)
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn correct_token_is_accepted() {
        let req = post(
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }),
            Some("secret"),
        );
        let resp = router(state(Some("secret".into())))
            .oneshot(req)
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn refuses_non_loopback_bind_without_token() {
        let opts = HttpOpts {
            addr: "8.8.8.8:9".parse().unwrap(),
            bearer_token: None,
        };
        let err = serve_http(Arc::new(MockEngine::new()), opts)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }
}
