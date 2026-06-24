//! Register an HTTP source on a DataFusion catalog.

use std::sync::Arc;

use datafusion::catalog::{
    CatalogProvider, MemoryCatalogProvider, MemorySchemaProvider, SchemaProvider,
};
use datafusion::execution::context::SessionContext;
use pawrly_core::{ConfigError, SourceDef};

use std::num::NonZeroU32;

use crate::raw::RawHttpTableProvider;
use crate::source::{AuthSpec, HttpSource, HttpTableSpec, RateLimitPolicy, RetryConfig};
use crate::typed::HttpTableProvider;

use governor::{Quota, RateLimiter};

#[derive(Debug, thiserror::Error)]
pub enum HttpBuildError {
    #[error("config error: {0}")]
    Config(#[from] ConfigError),

    #[error("invalid base_url: {0}")]
    BadUrl(String),

    #[error("openapi: {0}")]
    Spec(String),

    #[error("datafusion: {0}")]
    DataFusion(String),
}

/// Table count above which an OpenAPI source logs a hint to narrow its catalog.
const MANY_TABLES: usize = 50;

#[derive(Debug, Clone, Default)]
pub struct HttpSourceReport {
    pub table_count: u64,
    pub tables: Vec<HttpTableSummary>,
    pub raw_table_registered: bool,
    /// The live source handle, so attached functions can share the same
    /// rate-limiter / retry / auth state instead of opening a parallel client.
    /// `None` only on the `Default` used by error paths.
    pub source_handle: Option<Arc<HttpSource>>,
}

#[derive(Debug, Clone)]
pub struct HttpTableSummary {
    pub name: String,
    pub description: Option<String>,
    pub required_filters: Vec<String>,
}

pub async fn register_http_source(
    def: &SourceDef,
    ctx: &SessionContext,
    catalog: &dyn CatalogProvider,
) -> Result<HttpSourceReport, HttpBuildError> {
    let _ = ctx;

    let cfg = &def.config;
    let base_url_str = cfg
        .get("base_url")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if base_url_str.is_empty() {
        return Err(HttpBuildError::BadUrl(
            "`kind: http` requires `config.base_url`".to_string(),
        ));
    }

    let client = HttpSource::build_client();
    let source_max_pages = def.safety.as_ref().and_then(|s| s.max_pages);
    let source_max_rows = def.safety.as_ref().and_then(|s| s.max_rows);

    // In openapi mode `base_url` points at the spec, not the API; the request
    // base comes from the spec's `servers`.
    let (base_url, synthesized) = if cfg.get("type").and_then(|v| v.as_str()) == Some("openapi") {
        let bytes = fetch_spec(
            &client,
            &base_url_str,
            spec_cache(cfg, &base_url_str).as_ref(),
        )
        .await?;
        let doc: serde_json::Value = serde_yaml::from_slice(&bytes)
            .map_err(|e| HttpBuildError::Spec(format!("parse openapi document: {e}")))?;
        let synth = crate::openapi::synthesize(&doc, &openapi_options(cfg))
            .map_err(|e| HttpBuildError::Spec(e.to_string()))?;
        for d in &synth.diagnostics {
            tracing::warn!(source = %def.name, table = ?d.table, code = d.code, "{}", d.message);
        }
        if synth.tables.len() > MANY_TABLES {
            tracing::info!(
                source = %def.name,
                tables = synth.tables.len(),
                "openapi source registered many tables; set config.openapi.include to narrow the catalog"
            );
        }
        (effective_base(cfg, &synth, &base_url_str)?, synth.tables)
    } else {
        let base =
            url::Url::parse(&base_url_str).map_err(|e| HttpBuildError::BadUrl(e.to_string()))?;
        (base, Vec::new())
    };

    let source = Arc::new(HttpSource {
        name: def.name.clone(),
        base_url,
        auth: parse_auth(def),
        headers: parse_headers(def),
        client,
        retry: parse_retry(def),
        rate_limit: parse_rate_limit(def),
        oauth_token: tokio::sync::Mutex::new(None),
    });

    let schema = ensure_schema(catalog, &def.name)?;

    // Synthesized tables first. An explicit `def.tables` entry whose name matches
    // a synthesized table *patches* it (merge only the fields it sets); a new name
    // is a full table definition.
    let mut tables: Vec<(HttpTableSpec, Option<u32>, Option<u64>)> = synthesized
        .into_iter()
        .map(|spec| (spec, source_max_pages, source_max_rows))
        .collect();
    for t in &def.tables {
        let table_safety = t.safety.as_ref();
        let max_pages = table_safety.and_then(|s| s.max_pages).or(source_max_pages);
        let max_rows = table_safety.and_then(|s| s.max_rows).or(source_max_rows);
        match tables.iter_mut().find(|(s, _, _)| s.name == t.name) {
            Some(slot) => {
                slot.0 = merge_table_def(&slot.0, t)?;
                slot.1 = max_pages;
                slot.2 = max_rows;
            }
            None => tables.push((table_spec_from_def(t)?, max_pages, max_rows)),
        }
    }
    if tables.is_empty() && !def.raw_table {
        tracing::warn!(
            source = %def.name,
            "http source has no tables and raw_table is disabled; nothing to register"
        );
    }

    let mut summaries = Vec::with_capacity(tables.len());
    for (spec, max_pages, max_rows) in tables {
        let required: Vec<String> = spec
            .params
            .iter()
            .filter(|p| p.required)
            .map(|p| p.name.clone())
            .collect();
        let description = spec.description.clone();
        let provider = HttpTableProvider::with_safety(
            source.clone(),
            Arc::new(spec.clone()),
            max_pages,
            max_rows,
        );
        schema
            .register_table(spec.name.clone(), Arc::new(provider))
            .map_err(|e| HttpBuildError::DataFusion(format!("register table: {e}")))?;
        summaries.push(HttpTableSummary {
            name: spec.name,
            description,
            required_filters: required,
        });
    }

    // Optional raw HTTP table named after the source, registered in two places:
    //   * the `default` schema, so the unqualified `SELECT * FROM <source>`
    //     convenience resolves to it; and
    //   * the source's own schema as `<source>.<source>`, which is how the
    //     catalog lists it — so the advertised name is also the queryable name
    //     (and `describe_table` finds it directly).
    let raw_table_registered = if def.raw_table {
        let default_schema = catalog
            .schema("default")
            .ok_or_else(|| HttpBuildError::DataFusion("default schema missing".into()))?;
        default_schema
            .register_table(
                def.name.clone(),
                Arc::new(RawHttpTableProvider::new(source.clone())),
            )
            .map_err(|e| HttpBuildError::DataFusion(format!("register raw table: {e}")))?;
        schema
            .register_table(
                def.name.clone(),
                Arc::new(RawHttpTableProvider::new(source.clone())),
            )
            .map_err(|e| {
                HttpBuildError::DataFusion(format!("register raw table (qualified): {e}"))
            })?;
        true
    } else {
        false
    };

    Ok(HttpSourceReport {
        table_count: summaries.len() as u64 + raw_table_registered as u64,
        tables: summaries,
        raw_table_registered,
        source_handle: Some(source),
    })
}

/// Build a standalone [`HttpSource`] from a `(name, config)` pair — the
/// connection block of a standalone http function (`base_url`, `auth`,
/// `headers`, `retry`, `rate_limit`), reusing the same parsers as a source.
/// When `base_url` is absent the function's `endpoint` must be absolute (the
/// validator enforces this), so a placeholder base is used and overridden by the
/// absolute endpoint at request time.
pub fn build_http_source(
    name: &str,
    config: &serde_json::Value,
) -> Result<Arc<HttpSource>, HttpBuildError> {
    let base_url_str = config
        .get("base_url")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("http://localhost/");
    let base_url =
        url::Url::parse(base_url_str).map_err(|e| HttpBuildError::BadUrl(e.to_string()))?;
    let def = SourceDef {
        name: name.to_string(),
        kind: pawrly_core::SourceKind::Http,
        description: None,
        wiki: None,
        examples: Vec::new(),
        config: config.clone(),
        cache: Default::default(),
        safety: None,
        tables: Vec::new(),
        raw_table: false,
        raw_table_safety: None,
    };
    Ok(Arc::new(HttpSource {
        name: name.to_string(),
        base_url,
        auth: parse_auth(&def),
        headers: parse_headers(&def),
        client: HttpSource::build_client(),
        retry: parse_retry(&def),
        rate_limit: parse_rate_limit(&def),
        oauth_token: tokio::sync::Mutex::new(None),
    }))
}

/// Build an [`HttpTableSpec`] from a user-declared `TableDef`. The table's
/// `name` comes from the `TableDef`; the rest (`endpoint`, `method`, `params`,
/// `headers`, `response`) is read from its opaque `config` JSON. The table
/// name from the `config` body, if any, is overridden by the `TableDef.name`.
fn table_spec_from_def(t: &pawrly_core::TableDef) -> Result<HttpTableSpec, HttpBuildError> {
    let mut body = t.config.clone();
    if !body.is_object() {
        body = serde_json::Value::Object(serde_json::Map::new());
    }
    if let Some(map) = body.as_object_mut() {
        // The TableDef owns the canonical name; inject it so the user doesn't
        // repeat `name:` inside the table body.
        map.insert(
            "name".to_string(),
            serde_json::Value::String(t.name.clone()),
        );
        if let Some(desc) = &t.description {
            map.entry("description")
                .or_insert_with(|| serde_json::Value::String(desc.clone()));
        }
    }
    serde_json::from_value(body).map_err(|e| {
        HttpBuildError::Config(ConfigError::Source(
            t.name.clone(),
            format!("invalid http table `{}`: {e}", t.name),
        ))
    })
}

/// Patch a synthesized table with the fields a `TableDef` of the same name sets,
/// keeping the rest of the synthesis. The table body is deep-merged over the
/// synthesized spec (see [`pawrly_schema::deep_merge`]).
fn merge_table_def(
    base: &HttpTableSpec,
    t: &pawrly_core::TableDef,
) -> Result<HttpTableSpec, HttpBuildError> {
    let mut value = serde_json::to_value(base).map_err(|e| {
        HttpBuildError::Config(ConfigError::Source(
            t.name.clone(),
            format!("serialize synthesized table `{}`: {e}", t.name),
        ))
    })?;
    let mut patch = t.config.clone();
    if let Some(map) = patch.as_object_mut() {
        // `name` is the match key, not a patchable field.
        map.remove("name");
        if let Some(desc) = &t.description {
            map.entry("description")
                .or_insert_with(|| serde_json::Value::String(desc.clone()));
        }
    }
    pawrly_schema::deep_merge(&mut value, &patch);
    serde_json::from_value(value).map_err(|e| {
        HttpBuildError::Config(ConfigError::Source(
            t.name.clone(),
            format!("invalid patch for table `{}`: {e}", t.name),
        ))
    })
}

/// Resolve a source's auth from `config`. The `config.token` shorthand is a
/// single bearer header; otherwise the `config.auth` block deserializes into an
/// [`AuthSpec`] (`header` / `basic` / `custom` / `oauth2`). A malformed or
/// absent block falls back to no auth.
fn parse_auth(def: &SourceDef) -> AuthSpec {
    let cfg = &def.config;
    if let Some(token) = cfg.get("token").and_then(|v| v.as_str()) {
        return AuthSpec::bearer(token);
    }
    match cfg.get("auth") {
        Some(auth) => serde_json::from_value::<AuthSpec>(auth.clone()).unwrap_or(AuthSpec::None),
        None => AuthSpec::None,
    }
}

/// Parse static source-level request headers from `config.headers` (a
/// string→string map). These are attached to every request the source issues —
/// both typed and raw tables — *before* any per-table `headers`, so a table can
/// still override a source-level value. Entries with an invalid header name or
/// value are skipped (a malformed header should not fail the whole source).
fn parse_headers(def: &SourceDef) -> reqwest::header::HeaderMap {
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

    let mut headers = HeaderMap::new();
    let Some(map) = def.config.get("headers").and_then(|v| v.as_object()) else {
        return headers;
    };
    for (k, v) in map {
        let Some(val) = v.as_str() else {
            tracing::warn!(
                source = %def.name,
                header = %k,
                "config.headers value is not a string; skipping"
            );
            continue;
        };
        match (HeaderName::try_from(k.as_str()), HeaderValue::try_from(val)) {
            (Ok(name), Ok(value)) => {
                headers.insert(name, value);
            }
            _ => tracing::warn!(
                source = %def.name,
                header = %k,
                "invalid config.headers entry; skipping"
            ),
        }
    }
    headers
}

/// Parse the retry policy from `config.retry.{max_retries,base_backoff_ms,
/// max_backoff_ms}`, falling back to [`RetryConfig::default`] for absent fields.
fn parse_retry(def: &SourceDef) -> RetryConfig {
    let mut retry = RetryConfig::default();
    if let Some(r) = def.config.get("retry") {
        if let Some(v) = r.get("max_retries").and_then(serde_json::Value::as_u64) {
            retry.max_retries = v as u32;
        }
        if let Some(v) = r.get("base_backoff_ms").and_then(serde_json::Value::as_u64) {
            retry.base_backoff_ms = v;
        }
        if let Some(v) = r.get("max_backoff_ms").and_then(serde_json::Value::as_u64) {
            retry.max_backoff_ms = v;
        }
    }
    retry
}

/// Build the rate-limit policy from `config.rate_limit`: a direct (un-keyed)
/// token-bucket limiter from `requests_per_second`, plus the header-awareness
/// fields (`remaining_header`, `reset_header`, `extra_statuses`).
fn parse_rate_limit(def: &SourceDef) -> RateLimitPolicy {
    let Some(rl) = def.config.get("rate_limit") else {
        return RateLimitPolicy::default();
    };
    let limiter = rl
        .get("requests_per_second")
        .and_then(serde_json::Value::as_u64)
        .and_then(|rps| NonZeroU32::new(u32::try_from(rps).ok()?))
        .map(|rps| Arc::new(RateLimiter::direct(Quota::per_second(rps))));
    let header = |key: &str| rl.get(key).and_then(|v| v.as_str()).map(str::to_string);
    let extra_statuses = rl
        .get("extra_statuses")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| u16::try_from(v.as_u64()?).ok())
                .collect()
        })
        .unwrap_or_default();
    RateLimitPolicy {
        limiter,
        remaining_header: header("remaining_header"),
        reset_header: header("reset_header"),
        extra_statuses,
    }
}

/// Read the `config.openapi` synthesis options (`include`/`exclude`/`naming`).
fn openapi_options(cfg: &serde_json::Value) -> crate::openapi::SynthOptions {
    use crate::openapi::{Naming, Selector, SynthOptions};

    let block = cfg.get("openapi");
    let selector = |key: &str| -> Selector {
        let Some(sel) = block.and_then(|b| b.get(key)) else {
            return Selector::default();
        };
        let list = |field: &str| {
            sel.get(field)
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };
        Selector {
            tags: list("tags"),
            paths: list("paths"),
            operations: list("operations"),
        }
    };
    let naming = match block.and_then(|b| b.get("naming")).and_then(|v| v.as_str()) {
        Some("path") => Naming::Path,
        Some("tag") => Naming::Tag,
        _ => Naming::OperationId,
    };
    SynthOptions {
        include: selector("include"),
        exclude: selector("exclude"),
        naming,
    }
}

/// An on-disk cache for a fetched spec: where it lives and how long it stays fresh.
struct SpecCache {
    path: std::path::PathBuf,
    ttl: std::time::Duration,
}

/// Fetch spec bytes from an `http(s)://` URL or a `file://` path. When `cache` is
/// set and the on-disk copy is within its TTL, the network is skipped.
async fn fetch_spec(
    client: &reqwest::Client,
    location: &str,
    cache: Option<&SpecCache>,
) -> Result<Vec<u8>, HttpBuildError> {
    if let Some(path) = location.strip_prefix("file://") {
        return tokio::fs::read(path)
            .await
            .map_err(|e| HttpBuildError::Spec(format!("read spec `{path}`: {e}")));
    }
    if let Some(cache) = cache
        && let Some(bytes) = read_if_fresh(&cache.path, cache.ttl).await
    {
        return Ok(bytes);
    }
    let resp = client
        .get(location)
        .send()
        .await
        .map_err(|e| HttpBuildError::Spec(format!("fetch spec: {e}")))?;
    if !resp.status().is_success() {
        return Err(HttpBuildError::Spec(format!(
            "fetch spec returned HTTP {}",
            resp.status()
        )));
    }
    let bytes = resp
        .bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| HttpBuildError::Spec(format!("read spec body: {e}")))?;
    if let Some(cache) = cache {
        write_cache(&cache.path, &bytes).await;
    }
    Ok(bytes)
}

/// Resolve the spec cache for an openapi source, or `None` when caching is off
/// (no `config.openapi.cache.ttl`, a `file://` spec, or no home directory).
fn spec_cache(cfg: &serde_json::Value, location: &str) -> Option<SpecCache> {
    if location.starts_with("file://") {
        return None;
    }
    Some(SpecCache {
        ttl: spec_cache_ttl(cfg)?,
        path: spec_cache_path(location)?,
    })
}

/// Read `config.openapi.cache.ttl` as a humantime duration (e.g. `24h`).
fn spec_cache_ttl(cfg: &serde_json::Value) -> Option<std::time::Duration> {
    #[derive(serde::Deserialize)]
    struct Ttl {
        #[serde(with = "humantime_serde")]
        ttl: std::time::Duration,
    }
    let cache = cfg.get("openapi")?.get("cache")?;
    serde_json::from_value::<Ttl>(cache.clone())
        .ok()
        .map(|t| t.ttl)
}

/// `$PAWRLY_HOME/cache/openapi/<hash>.spec` (falling back to `~/.pawrly`), keyed
/// by the spec URL so distinct specs never collide.
fn spec_cache_path(location: &str) -> Option<std::path::PathBuf> {
    use std::hash::{Hash as _, Hasher as _};

    let base = std::env::var_os("PAWRLY_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".pawrly"))
        })?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    location.hash(&mut hasher);
    Some(
        base.join("cache")
            .join("openapi")
            .join(format!("{:016x}.spec", hasher.finish())),
    )
}

/// The cached spec if it exists and was written within `ttl`.
async fn read_if_fresh(path: &std::path::Path, ttl: std::time::Duration) -> Option<Vec<u8>> {
    let modified = tokio::fs::metadata(path).await.ok()?.modified().ok()?;
    if modified.elapsed().ok()? <= ttl {
        tokio::fs::read(path).await.ok()
    } else {
        None
    }
}

/// Best-effort write of a fetched spec to the cache; failures are non-fatal.
async fn write_cache(path: &std::path::Path, bytes: &[u8]) {
    if let Some(parent) = path.parent()
        && tokio::fs::create_dir_all(parent).await.is_err()
    {
        return;
    }
    if let Err(e) = tokio::fs::write(path, bytes).await {
        tracing::debug!(error = %e, "failed to write openapi spec cache");
    }
}

/// The effective request base for an OpenAPI source: a `config.openapi.base_url`
/// override wins, then the spec's `servers[0].url` (resolved against the spec
/// origin when relative), then the origin of the spec URL.
fn effective_base(
    cfg: &serde_json::Value,
    synth: &crate::openapi::Synthesis,
    spec_location: &str,
) -> Result<url::Url, HttpBuildError> {
    if let Some(over) = cfg
        .get("openapi")
        .and_then(|b| b.get("base_url"))
        .and_then(|v| v.as_str())
    {
        return url::Url::parse(over).map_err(|e| HttpBuildError::BadUrl(e.to_string()));
    }
    if let Some(server) = &synth.base_url {
        if let Ok(u) = url::Url::parse(server) {
            return Ok(u);
        }
        if let Ok(joined) = url::Url::parse(spec_location).and_then(|s| s.join(server)) {
            return Ok(joined);
        }
    }
    let mut origin =
        url::Url::parse(spec_location).map_err(|e| HttpBuildError::BadUrl(e.to_string()))?;
    origin.set_path("/");
    origin.set_query(None);
    origin.set_fragment(None);
    Ok(origin)
}

fn ensure_schema(
    catalog: &dyn CatalogProvider,
    name: &str,
) -> Result<Arc<dyn SchemaProvider>, HttpBuildError> {
    if let Some(s) = catalog.schema(name) {
        return Ok(s);
    }
    let s: Arc<dyn SchemaProvider> = Arc::new(MemorySchemaProvider::new());
    if let Some(memory_catalog) = catalog.as_any().downcast_ref::<MemoryCatalogProvider>() {
        let _ = memory_catalog
            .register_schema(name, s.clone())
            .map_err(|e| HttpBuildError::DataFusion(e.to_string()))?;
        Ok(s)
    } else {
        Err(HttpBuildError::DataFusion(
            "catalog does not support schema registration".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    #[test]
    fn spec_cache_ttl_reads_humantime() {
        assert_eq!(
            spec_cache_ttl(&json!({ "openapi": { "cache": { "ttl": "24h" } } })),
            Some(Duration::from_secs(86_400))
        );
        assert_eq!(spec_cache_ttl(&json!({ "openapi": {} })), None);
        assert_eq!(
            spec_cache_ttl(&json!({ "openapi": { "cache": { "ttl": "nonsense" } } })),
            None
        );
    }

    #[test]
    fn spec_cache_is_off_for_file_urls() {
        let cfg = json!({ "openapi": { "cache": { "ttl": "1h" } } });
        assert!(spec_cache(&cfg, "file:///tmp/spec.yaml").is_none());
    }

    #[tokio::test]
    async fn read_if_fresh_honors_ttl() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("spec");
        write_cache(&path, b"openapi: 3.0.0").await;

        assert_eq!(
            read_if_fresh(&path, Duration::from_secs(3600))
                .await
                .as_deref(),
            Some(&b"openapi: 3.0.0"[..])
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
        assert_eq!(read_if_fresh(&path, Duration::ZERO).await, None);
        assert_eq!(
            read_if_fresh(&dir.path().join("missing"), Duration::from_secs(60)).await,
            None
        );
    }

    /// A fresh cache short-circuits the network: the server answers once, yet a
    /// second fetch still succeeds from disk.
    #[tokio::test]
    async fn fetch_spec_skips_network_when_cached() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/spec"))
            .respond_with(ResponseTemplate::new(200).set_body_string("openapi: 3.0.0"))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("tempdir");
        let cache = SpecCache {
            path: dir.path().join("s.spec"),
            ttl: Duration::from_secs(3600),
        };
        let client = HttpSource::build_client();
        let url = format!("{}/spec", server.uri());

        let first = fetch_spec(&client, &url, Some(&cache))
            .await
            .expect("first fetch");
        let second = fetch_spec(&client, &url, Some(&cache))
            .await
            .expect("cached fetch");
        assert_eq!(first, b"openapi: 3.0.0");
        assert_eq!(second, first);
    }
}
