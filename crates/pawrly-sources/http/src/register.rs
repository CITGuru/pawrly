//! Register an HTTP source on a DataFusion catalog.

use std::sync::Arc;

use datafusion::catalog::{
    CatalogProvider, MemoryCatalogProvider, MemorySchemaProvider, SchemaProvider,
};
use datafusion::execution::context::SessionContext;
use pawrly_core::{ConfigError, SourceDef};

use crate::bundled;
use crate::raw::RawHttpTableProvider;
use crate::source::{AuthSpec, HttpSource, HttpTableSpec};
use crate::typed::HttpTableProvider;

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

    let source = Arc::new(HttpSource {
        name: def.name.clone(),
        base_url,
        auth,
        headers,
        client: HttpSource::build_client(),
    });

    // 2. Ensure the schema provider exists on the catalog.
    let schema = ensure_schema(catalog, &def.name)?;

    // 3. Combine tables from the bundled spec + any user table overrides.
    //    User overrides not yet merged; we use bundled tables as-is
    //    when the source is bundled, else expect user-declared tables (none
    //    declared → empty).
    let tables: Vec<HttpTableSpec> = if !default_tables.is_empty() {
        default_tables
    } else {
        // Generic kind: http — user must declare tables under `tables:`.
        // Not yet wired; surface a clear message.
        if def.tables.is_empty() && !def.raw_table {
            tracing::warn!(
                source = %def.name,
                "http source has no tables and raw_table is disabled; nothing to register"
            );
        }
        Vec::new()
    };

    let mut summaries = Vec::with_capacity(tables.len());
    for spec in tables {
        let required: Vec<String> = spec
            .params
            .iter()
            .filter(|p| p.required)
            .map(|p| p.name.clone())
            .collect();
        let description = spec.description.clone();
        let provider = HttpTableProvider::new(source.clone(), Arc::new(spec.clone()));
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
