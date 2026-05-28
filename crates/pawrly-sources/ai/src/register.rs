//! Register an AI source on a DataFusion catalog.

use std::sync::Arc;

use datafusion::catalog::{
    CatalogProvider, MemoryCatalogProvider, MemorySchemaProvider, SchemaProvider,
};
use datafusion::execution::context::SessionContext;
use pawrly_core::{ConfigError, SourceDef};

use crate::models_table::{ModelRow, ModelsTable};
use crate::udf::ChatUdf;

#[derive(Debug, thiserror::Error)]
pub enum AiBuildError {
    #[error("config error: {0}")]
    Config(#[from] ConfigError),

    #[error("invalid base_url: {0}")]
    BadUrl(String),

    #[error("`provider` is required for kind: ai")]
    MissingProvider,

    #[error("datafusion: {0}")]
    DataFusion(String),
}

#[derive(Debug, Clone, Default)]
pub struct AiSourceReport {
    pub table_count: u64,
    pub udfs_registered: Vec<String>,
}

pub async fn register_ai_source(
    def: &SourceDef,
    ctx: &SessionContext,
    catalog: &dyn CatalogProvider,
) -> Result<AiSourceReport, AiBuildError> {
    let cfg = &def.config;
    let provider = cfg
        .get("provider")
        .and_then(|v| v.as_str())
        .ok_or(AiBuildError::MissingProvider)?
        .to_string();
    let base_url_str = cfg
        .get("base_url")
        .and_then(|v| v.as_str())
        .unwrap_or("https://api.openai.com")
        .to_string();
    let _ = provider.clone();
    let base_url =
        url::Url::parse(&base_url_str).map_err(|e| AiBuildError::BadUrl(e.to_string()))?;
    let api_key = cfg
        .get("api_key")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let default_model = cfg
        .get("default_model")
        .and_then(|v| v.as_str())
        .unwrap_or("gpt-5")
        .to_string();

    let client = reqwest::Client::builder()
        .user_agent(format!("pawrly/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let schema = ensure_schema(catalog, &def.name)?;

    // Register the `<source>.models` table.
    let models = ModelsTable::new(vec![ModelRow {
        name: "default".into(),
        model: default_model,
        provider: provider.clone(),
    }]);
    schema
        .register_table("models".into(), Arc::new(models))
        .map_err(|e| AiBuildError::DataFusion(format!("register models: {e}")))?;

    // Register the `<source>.chat` UDF.
    let chat = ChatUdf::build(&def.name, base_url, api_key, client);
    ctx.register_udf(chat);

    Ok(AiSourceReport {
        table_count: 1,
        udfs_registered: vec![format!("{}.chat", def.name)],
    })
}

fn ensure_schema(
    catalog: &dyn CatalogProvider,
    name: &str,
) -> Result<Arc<dyn SchemaProvider>, AiBuildError> {
    if let Some(s) = catalog.schema(name) {
        return Ok(s);
    }
    let s: Arc<dyn SchemaProvider> = Arc::new(MemorySchemaProvider::new());
    if let Some(memory_catalog) = catalog.as_any().downcast_ref::<MemoryCatalogProvider>() {
        let _ = memory_catalog
            .register_schema(name, s.clone())
            .map_err(|e| AiBuildError::DataFusion(e.to_string()))?;
        Ok(s)
    } else {
        Err(AiBuildError::DataFusion(
            "catalog does not support schema registration".into(),
        ))
    }
}
