//! Register an MCP source: connect, discover tools, synthesize tables, and back
//! each with an [`McpToolTableProvider`] sharing one session.

use std::sync::Arc;

use datafusion::catalog::{
    CatalogProvider, MemoryCatalogProvider, MemorySchemaProvider, SchemaProvider,
};
use datafusion::execution::context::SessionContext;
use pawrly_core::SourceDef;
use serde_json::Value;

use crate::error::McpBuildError;
use crate::http::{HttpTransport, auth_headers};
use crate::provider::McpToolTableProvider;
use crate::session::McpClientSession;
use crate::stdio::StdioTransport;
use crate::synth::{Expose, SynthOptions, apply_table_def, synthesize_tools};
use crate::transport::McpTransport;

#[derive(Debug, Clone, Default)]
pub struct McpSourceReport {
    pub table_count: u64,
    pub tables: Vec<McpTableSummary>,
    /// The live client session, so attached functions share one connection with
    /// the source (like the http source handle). `None` only on the `Default`
    /// used by error paths.
    pub session_handle: Option<Arc<McpClientSession>>,
}

#[derive(Debug, Clone)]
pub struct McpTableSummary {
    pub name: String,
    pub description: Option<String>,
}

pub async fn register_mcp_source(
    def: &SourceDef,
    _ctx: &SessionContext,
    catalog: &dyn CatalogProvider,
) -> Result<McpSourceReport, McpBuildError> {
    let cfg = &def.config;
    let transport = build_transport(cfg)?;
    let session = Arc::new(
        McpClientSession::connect(transport)
            .await
            .map_err(|e| McpBuildError::Connect(e.to_string()))?,
    );
    let tools = session
        .list_tools()
        .await
        .map_err(|e| McpBuildError::Connect(e.to_string()))?;

    let mut synth = synthesize_tools(&tools, &synth_options(cfg));
    let tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
    for t in &def.tables {
        apply_table_def(
            &mut synth.tables,
            &t.name,
            t.description.as_deref(),
            &t.config,
            &tool_names,
            &mut synth.diagnostics,
        )
        .map_err(|e| McpBuildError::Config(format!("table `{}`: {e}", t.name)))?;
    }
    for d in &synth.diagnostics {
        tracing::warn!(source = %def.name, table = ?d.table, code = d.code, "{}", d.message);
    }

    let schema = ensure_schema(catalog, &def.name)?;
    let max_pages = def.safety.as_ref().and_then(|s| s.max_pages);
    let mut summaries = Vec::with_capacity(synth.tables.len());
    for spec in synth.tables {
        let summary = McpTableSummary {
            name: spec.name.clone(),
            description: spec.description.clone(),
        };
        let provider = McpToolTableProvider::new(session.clone(), Arc::new(spec), max_pages);
        schema
            .register_table(summary.name.clone(), Arc::new(provider))
            .map_err(|e| McpBuildError::DataFusion(format!("register table: {e}")))?;
        summaries.push(summary);
    }

    Ok(McpSourceReport {
        table_count: summaries.len() as u64,
        tables: summaries,
        session_handle: Some(session),
    })
}

/// Connect a standalone [`McpClientSession`] from a function's `config`
/// connection block (`transport` + `command`/`url`), reusing the same transport
/// builder as a `kind: mcp` source.
pub async fn build_mcp_session(config: &Value) -> Result<Arc<McpClientSession>, McpBuildError> {
    let transport = build_transport(config)?;
    let session = McpClientSession::connect(transport)
        .await
        .map_err(|e| McpBuildError::Connect(e.to_string()))?;
    Ok(Arc::new(session))
}

fn build_transport(cfg: &Value) -> Result<Arc<dyn McpTransport>, McpBuildError> {
    match cfg.get("transport").and_then(Value::as_str) {
        Some("stdio") => {
            let (command, args) = parse_command(cfg)?;
            let env = parse_env(cfg);
            let transport = StdioTransport::spawn(&command, &args, &env)
                .map_err(|e| McpBuildError::Connect(e.to_string()))?;
            Ok(Arc::new(transport))
        }
        Some("streamable_http") => {
            let url = cfg
                .get("url")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    McpBuildError::Config("`streamable_http` requires `config.url`".into())
                })?
                .to_string();
            let auth = cfg.get("auth").map(auth_headers).unwrap_or_default();
            Ok(Arc::new(HttpTransport::new(url, auth)))
        }
        other => Err(McpBuildError::Config(format!(
            "`kind: mcp` requires `config.transport` of `stdio` or `streamable_http` (got {other:?})"
        ))),
    }
}

/// `command` is a `[program, args…]` array, or a string program with `config.args`.
fn parse_command(cfg: &Value) -> Result<(String, Vec<String>), McpBuildError> {
    match cfg.get("command") {
        Some(Value::Array(parts)) => {
            let mut iter = parts.iter().filter_map(Value::as_str);
            let program = iter
                .next()
                .ok_or_else(|| McpBuildError::Config("`config.command` array is empty".into()))?
                .to_string();
            Ok((program, iter.map(str::to_string).collect()))
        }
        Some(Value::String(program)) => {
            let args = cfg
                .get("args")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            Ok((program.clone(), args))
        }
        _ => Err(McpBuildError::Config(
            "`transport: stdio` requires `config.command`".into(),
        )),
    }
}

fn parse_env(cfg: &Value) -> Vec<(String, String)> {
    cfg.get("env")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

fn synth_options(cfg: &Value) -> SynthOptions {
    let expose = match cfg.get("expose").and_then(Value::as_str) {
        Some("all") => Expose::All,
        Some("listed") => Expose::Listed,
        _ => Expose::ReadOnly,
    };
    let tools = |key: &str| {
        cfg.get(key)
            .and_then(|v| v.get("tools"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    SynthOptions {
        expose,
        include: tools("include"),
        exclude: tools("exclude"),
    }
}

fn ensure_schema(
    catalog: &dyn CatalogProvider,
    name: &str,
) -> Result<Arc<dyn SchemaProvider>, McpBuildError> {
    if let Some(schema) = catalog.schema(name) {
        return Ok(schema);
    }
    let schema: Arc<dyn SchemaProvider> = Arc::new(MemorySchemaProvider::new());
    let memory = catalog
        .as_any()
        .downcast_ref::<MemoryCatalogProvider>()
        .ok_or_else(|| {
            McpBuildError::DataFusion("catalog does not support schema registration".into())
        })?;
    memory
        .register_schema(name, schema.clone())
        .map_err(|e| McpBuildError::DataFusion(e.to_string()))?;
    Ok(schema)
}
