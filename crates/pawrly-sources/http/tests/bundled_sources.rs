//! Acceptance for the bundled sources (`sentry`, `slack`) and the
//! expanded `github` bundle: link-header + cursor pagination, bearer auth,
//! nested `$.a.b` column extraction, default query params, required-filter
//! errors, and `safety.max_pages` enforcement.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::sync::Arc;

use arrow_array::{BooleanArray, Int64Array, StringArray};
use datafusion::catalog::{CatalogProvider, MemoryCatalogProvider};
use datafusion::execution::config::SessionConfig;
use datafusion::execution::context::SessionContext;
use pawrly_core::{CachePolicy, SafetyPolicy, SourceDef, SourceKind};
use pawrly_sources_http::register_http_source;
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn build_ctx() -> (SessionContext, Arc<MemoryCatalogProvider>) {
    let cfg = SessionConfig::new()
        .with_default_catalog_and_schema("pawrly", "default")
        .with_create_default_catalog_and_schema(false);
    let ctx = SessionContext::new_with_config(cfg);
    let catalog: Arc<MemoryCatalogProvider> = Arc::new(MemoryCatalogProvider::new());
    let default_schema: Arc<dyn datafusion::catalog::SchemaProvider> =
        Arc::new(datafusion::catalog::MemorySchemaProvider::new());
    let _ = catalog.register_schema("default", default_schema).unwrap();
    ctx.register_catalog("pawrly", catalog.clone());
    (ctx, catalog)
}

fn source(name: &str, kind: SourceKind, server_uri: &str) -> SourceDef {
    SourceDef {
        name: name.into(),
        kind,
        description: None,
        config: json!({ "base_url": server_uri, "token": "test-token" }),
        cache: CachePolicy::None,
        safety: None,
        tables: Vec::new(),
        raw_table: false,
        raw_table_safety: None,
    }
}

async fn rows(ctx: &SessionContext, sql: &str) -> Vec<arrow_array::RecordBatch> {
    ctx.sql(sql)
        .await
        .expect("plan")
        .collect()
        .await
        .expect("execute")
}

fn total_rows(batches: &[arrow_array::RecordBatch]) -> usize {
    batches.iter().map(|b| b.num_rows()).sum()
}

// ---- sentry: link-header pagination, bearer auth, nested columns -----------

#[tokio::test]
async fn sentry_issues_link_header_pagination() {
    let server = MockServer::start().await;
    // Page 1: at the issues endpoint, with a `Link` pointing at /p2 and
    // results="true" (follow it). Auth header is asserted here.
    Mock::given(method("GET"))
        .and(path("/api/0/organizations/acme/issues/"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(
                    "Link",
                    format!("<{}/p2>; rel=\"next\"; results=\"true\"", server.uri()).as_str(),
                )
                .set_body_json(json!([
                    { "id": "1", "shortId": "ACME-1", "title": "boom", "status": "unresolved",
                      "level": "error", "count": 7, "userCount": 3, "firstSeen": "t0",
                      "lastSeen": "t1", "project": { "slug": "web" } },
                    { "id": "2", "shortId": "ACME-2", "title": "bang", "status": "unresolved",
                      "level": "warning", "count": 1, "userCount": 1, "firstSeen": "t0",
                      "lastSeen": "t1", "project": { "slug": "api" } }
                ])),
        )
        .mount(&server)
        .await;
    // Page 2: terminal — results="false" stops the walk.
    Mock::given(method("GET"))
        .and(path("/p2"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(
                    "Link",
                    format!("<{}/p2>; rel=\"next\"; results=\"false\"", server.uri()).as_str(),
                )
                .set_body_json(json!([
                    { "id": "3", "shortId": "ACME-3", "title": "thud", "status": "unresolved",
                      "level": "error", "count": 9, "userCount": 4, "firstSeen": "t0",
                      "lastSeen": "t1", "project": { "slug": "web" } }
                ])),
        )
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let def = source("sentry", SourceKind::Sentry, &server.uri());
    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register sentry");

    let batches = rows(
        &ctx,
        "SELECT id, count, project, org FROM sentry.issues WHERE org = 'acme' ORDER BY id",
    )
    .await;
    assert_eq!(total_rows(&batches), 3, "should union both pages");

    // Nested + typed extraction: `count` (bigint) and `project` ($.project.slug)
    // and the injected `org` param column.
    let b = &batches[0];
    let counts = b
        .column(1)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("count int64");
    assert_eq!(counts.value(0), 7);
    let project = b
        .column(2)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("project utf8");
    assert_eq!(project.value(0), "web");
    let org = b
        .column(3)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("org utf8");
    assert_eq!(org.value(0), "acme");
}

#[tokio::test]
async fn sentry_issues_requires_org() {
    let server = MockServer::start().await;
    let (ctx, catalog) = build_ctx().await;
    let def = source("sentry", SourceKind::Sentry, &server.uri());
    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register");

    let err = ctx
        .sql("SELECT * FROM sentry.issues")
        .await
        .expect("plan")
        .collect()
        .await
        .expect_err("missing org should error");
    assert!(
        err.to_string().contains("PAWRLY_SAFETY_REQUIRED_FILTER"),
        "unexpected: {err}"
    );
}

#[tokio::test]
async fn sentry_link_header_respects_max_pages() {
    let server = MockServer::start().await;
    // An endless `results="true"` Link would loop forever; max_pages must stop it.
    Mock::given(method("GET"))
        .and(path("/api/0/organizations/acme/issues/"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(
                    "Link",
                    format!("<{}/loop>; rel=\"next\"; results=\"true\"", server.uri()).as_str(),
                )
                .set_body_json(json!([{ "id": "1", "title": "x" }])),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/loop"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(
                    "Link",
                    format!("<{}/loop>; rel=\"next\"; results=\"true\"", server.uri()).as_str(),
                )
                .set_body_json(json!([{ "id": "2", "title": "y" }])),
        )
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let mut def = source("sentry", SourceKind::Sentry, &server.uri());
    def.safety = Some(SafetyPolicy {
        max_pages: Some(2),
        ..Default::default()
    });
    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register");

    let err = ctx
        .sql("SELECT id FROM sentry.issues WHERE org = 'acme'")
        .await
        .expect("plan")
        .collect()
        .await
        .expect_err("endless pagination should hit max_pages");
    assert!(
        err.to_string().contains("page more than 2 times"),
        "unexpected: {err}"
    );
}

// ---- slack: cursor pagination, nested columns, bool, default param ---------

#[tokio::test]
async fn slack_users_cursor_pagination() {
    let server = MockServer::start().await;
    // Page 1: no cursor yet; carries next_cursor.
    Mock::given(method("GET"))
        .and(path("/api/users.list"))
        .and(query_param_is_missing("cursor"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "members": [
                { "id": "U1", "name": "ada", "real_name": "Ada",
                  "profile": { "display_name": "ada", "email": "ada@x.io" },
                  "is_bot": false, "is_admin": true, "deleted": false },
                { "id": "U2", "name": "bot", "real_name": "Bot",
                  "profile": { "display_name": "bot", "email": "bot@x.io" },
                  "is_bot": true, "is_admin": false, "deleted": false }
            ],
            "response_metadata": { "next_cursor": "c2" }
        })))
        .mount(&server)
        .await;
    // Page 2: cursor echoed back; empty next_cursor stops the walk.
    Mock::given(method("GET"))
        .and(path("/api/users.list"))
        .and(query_param("cursor", "c2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "members": [
                { "id": "U3", "name": "cleo", "real_name": "Cleo",
                  "profile": { "display_name": "cleo", "email": "cleo@x.io" },
                  "is_bot": false, "is_admin": false, "deleted": true }
            ],
            "response_metadata": { "next_cursor": "" }
        })))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let def = source("slack", SourceKind::Slack, &server.uri());
    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register slack");

    let batches = rows(
        &ctx,
        "SELECT id, email, is_bot FROM slack.users ORDER BY id",
    )
    .await;
    assert_eq!(total_rows(&batches), 3, "should union both cursor pages");

    let b = &batches[0];
    let email = b
        .column(1)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("email utf8");
    assert_eq!(email.value(0), "ada@x.io", "nested $.profile.email");
    let is_bot = b
        .column(2)
        .as_any()
        .downcast_ref::<BooleanArray>()
        .expect("is_bot bool");
    assert!(!is_bot.value(0));
    assert!(is_bot.value(1), "U2 is a bot");
}

#[tokio::test]
async fn slack_channels_sends_default_types_param() {
    let server = MockServer::start().await;
    // The default `types` param must reach the API as a query parameter.
    Mock::given(method("GET"))
        .and(path("/api/conversations.list"))
        .and(query_param("types", "public_channel,private_channel"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "channels": [
                { "id": "C1", "name": "general", "topic": { "value": "all" },
                  "purpose": { "value": "chat" }, "num_members": 12,
                  "is_archived": false, "created": 1700000000 }
            ],
            "response_metadata": { "next_cursor": "" }
        })))
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let def = source("slack", SourceKind::Slack, &server.uri());
    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register slack");

    let batches = rows(&ctx, "SELECT id, topic, num_members FROM slack.channels").await;
    assert_eq!(total_rows(&batches), 1);
    let topic = batches[0]
        .column(1)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("topic utf8");
    assert_eq!(topic.value(0), "all", "nested $.topic.value");
}

// ---- github: expanded bundle + link-header pagination ----------------------

#[tokio::test]
async fn github_issues_link_header_pagination() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/pawrly/pawrly/issues"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(
                    "Link",
                    format!("<{}/page2>; rel=\"next\"", server.uri()).as_str(),
                )
                .set_body_json(json!([
                    { "number": 1, "title": "a", "state": "open",
                      "html_url": "u1", "user": { "login": "x" }, "comments": 2 }
                ])),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/page2"))
        .respond_with(
            // No `next` rel → last page.
            ResponseTemplate::new(200).set_body_json(json!([
                { "number": 2, "title": "b", "state": "open",
                  "html_url": "u2", "user": { "login": "y" }, "comments": 0 }
            ])),
        )
        .mount(&server)
        .await;

    let (ctx, catalog) = build_ctx().await;
    let def = source("gh", SourceKind::Github, &server.uri());
    register_http_source(&def, &ctx, catalog.as_ref())
        .await
        .expect("register github");

    let batches = rows(
        &ctx,
        "SELECT number, title, user FROM gh.issues WHERE owner = 'pawrly' AND repo = 'pawrly' ORDER BY number",
    )
    .await;
    assert_eq!(total_rows(&batches), 2, "should union both link pages");
    let user = batches[0]
        .column(2)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("user utf8");
    assert_eq!(user.value(0), "x", "nested $.user.login");
}

// ---- registration: bundled tables are advertised --------------------------

#[tokio::test]
async fn bundled_kinds_register_expected_tables() {
    let server = MockServer::start().await;
    for (kind, expected) in [
        (SourceKind::Github, &["issues", "pulls"][..]),
        (SourceKind::Sentry, &["issues", "projects"][..]),
        (SourceKind::Slack, &["channels", "users"][..]),
    ] {
        let (ctx, catalog) = build_ctx().await;
        let def = source("s", kind, &server.uri());
        let report = register_http_source(&def, &ctx, catalog.as_ref())
            .await
            .expect("register");
        let mut names: Vec<String> = report.tables.iter().map(|t| t.name.clone()).collect();
        names.sort();
        for want in expected {
            assert!(
                names.iter().any(|n| n == want),
                "kind {kind:?} missing table `{want}`; got {names:?}"
            );
        }
    }
}
