//! Register an HTTP source on a DataFusion catalog.

use std::sync::Arc;

use datafusion::catalog::{
    CatalogProvider, MemoryCatalogProvider, MemorySchemaProvider, SchemaProvider,
};
use datafusion::execution::context::SessionContext;
use pawrly_core::{ConfigError, SourceDef};

use std::num::NonZeroU32;

use crate::bundled;
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

    #[error("datafusion: {0}")]
    DataFusion(String),
}

#[derive(Debug, Clone, Default)]
pub struct HttpSourceReport {
    pub table_count: u64,
    pub tables: Vec<HttpTableSummary>,
    pub raw_table_registered: bool,
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

    // 1. Resolve base URL + auth from def.config.
    let cfg = &def.config;

    // Bundled sources merge their YAML spec with the user-supplied config
    // (which only contains credentials / overrides).
    let (base_url_str, default_tables, default_raw_table, default_headers) =
        match bundled::for_kind(def.kind) {
            Some(spec) => (
                spec.base_url.clone(),
                spec.tables.clone(),
                spec.raw_table_default,
                spec.default_headers.clone(),
            ),
            None => (
                cfg.get("base_url")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                Vec::new(),
                false,
                Default::default(),
            ),
        };
    let base_url_override = cfg
        .get("base_url")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let base_url_str = base_url_override.unwrap_or(base_url_str);
    let base_url =
        url::Url::parse(&base_url_str).map_err(|e| HttpBuildError::BadUrl(e.to_string()))?;

    let auth = parse_auth(def);

    let mut headers = reqwest::header::HeaderMap::new();
    for (k, v) in &default_headers {
        if let (Ok(name), Ok(val)) = (
            k.parse::<reqwest::header::HeaderName>(),
            v.parse::<reqwest::header::HeaderValue>(),
        ) {
            headers.insert(name, val);
        }
    }

    let retry = parse_retry(def);
    let rate_limit = parse_rate_limit(def);

    let source = Arc::new(HttpSource {
        name: def.name.clone(),
        base_url,
        auth,
        headers,
        client: HttpSource::build_client(),
        retry,
        rate_limit,
    });

    // 2. Ensure the schema provider exists on the catalog.
    let schema = ensure_schema(catalog, &def.name)?;

    // 3. Combine tables from the bundled spec + any user table overrides.
    //    User overrides not yet merged; we use bundled tables as-is
    //    when the source is bundled, else expect user-declared tables (none
    //    declared → empty).
    // Bundled kinds ship their own table specs. Generic `kind: http` (and any
    // user-declared tables on a bundled kind) are read from `def.tables`, whose
    // opaque per-table `config` deserializes into an `HttpTableSpec`.
    // The effective page cap falls back to the source-level safety policy when
    // a table has no policy of its own.
    let source_max_pages = def.safety.as_ref().and_then(|s| s.max_pages);

    let mut tables: Vec<(HttpTableSpec, Option<u32>)> = default_tables
        .into_iter()
        .map(|spec| (spec, source_max_pages))
        .collect();
    for t in &def.tables {
        let max_pages = t
            .safety
            .as_ref()
            .and_then(|s| s.max_pages)
            .or(source_max_pages);
        tables.push((table_spec_from_def(t)?, max_pages));
    }
    if tables.is_empty() && !def.raw_table {
        tracing::warn!(
            source = %def.name,
            "http source has no tables and raw_table is disabled; nothing to register"
        );
    }

    let mut summaries = Vec::with_capacity(tables.len());
    for (spec, max_pages) in tables {
        let required: Vec<String> = spec
            .params
            .iter()
            .filter(|p| p.required)
            .map(|p| p.name.clone())
            .collect();
        let description = spec.description.clone();
        let provider =
            HttpTableProvider::with_max_pages(source.clone(), Arc::new(spec.clone()), max_pages);
        schema
            .register_table(spec.name.clone(), Arc::new(provider))
            .map_err(|e| HttpBuildError::DataFusion(format!("register table: {e}")))?;
        summaries.push(HttpTableSummary {
            name: spec.name,
            description,
            required_filters: required,
        });
    }

    // 4. Optional raw HTTP table named after the source — registered in the
    //    *default* schema so `SELECT * FROM <source>` resolves to it.
    let want_raw = def.raw_table || default_raw_table;
    let raw_table_registered = if want_raw {
        let default_schema = catalog
            .schema("default")
            .ok_or_else(|| HttpBuildError::DataFusion("default schema missing".into()))?;
        let raw = RawHttpTableProvider::new(source.clone());
        default_schema
            .register_table(def.name.clone(), Arc::new(raw))
            .map_err(|e| HttpBuildError::DataFusion(format!("register raw table: {e}")))?;
        true
    } else {
        false
    };

    Ok(HttpSourceReport {
        table_count: summaries.len() as u64 + raw_table_registered as u64,
        tables: summaries,
        raw_table_registered,
    })
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

fn parse_auth(def: &SourceDef) -> AuthSpec {
    let cfg = &def.config;
    if let Some(token) = cfg.get("token").and_then(|v| v.as_str()) {
        return AuthSpec::Bearer {
            token: token.to_string(),
        };
    }
    if let Some(api_key) = cfg.get("api_key").and_then(|v| v.as_str()) {
        return AuthSpec::Bearer {
            token: api_key.to_string(),
        };
    }
    if let Some(auth) = cfg.get("auth")
        && let Some(t) = auth.get("type").and_then(|v| v.as_str())
    {
        match t {
            "bearer" => {
                if let Some(token) = auth.get("token").and_then(|v| v.as_str()) {
                    return AuthSpec::Bearer {
                        token: token.to_string(),
                    };
                }
            }
            "api_key" => {
                let header = auth
                    .get("header")
                    .and_then(|v| v.as_str())
                    .unwrap_or("X-API-Key")
                    .to_string();
                if let Some(value) = auth.get("value").and_then(|v| v.as_str()) {
                    return AuthSpec::ApiKey {
                        header,
                        value: value.to_string(),
                    };
                }
            }
            "basic" => {
                let user = auth.get("username").and_then(|v| v.as_str()).unwrap_or("");
                let pass = auth.get("password").and_then(|v| v.as_str()).unwrap_or("");
                return AuthSpec::Basic {
                    username: user.to_string(),
                    password: pass.to_string(),
                };
            }
            _ => {}
        }
    }
    AuthSpec::None
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
    let header = |key: &str| {
        rl.get(key)
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
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
