//! Table-valued functions: registry, SQL rewrite, UDTF, and per-kind executors.
//!
//! A function call `FROM ns.fn(args)` is rewritten ([`rewrite`]) to the mangled
//! UDTF name `pawrly_fn__ns__fn`, which DataFusion plans via
//! [`udtf::PawrlyFunctionUdtf`]; binding the literal args yields a
//! [`udtf::FunctionCallTable`] whose `scan` calls the kind-specific executor.

mod file;
mod rewrite;
mod udtf;

pub(crate) use rewrite::rewrite_function_calls;
pub(crate) use udtf::PawrlyFunctionUdtf;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;
use pawrly_core::{EngineError, FunctionDef, FunctionDescription, FunctionInfo, FunctionKind};

/// The reserved UDTF name for a function. `__` is banned in user identifiers by
/// validation, so this can never collide with a user UDTF.
pub(crate) fn mangle(namespace: &str, name: &str) -> String {
    format!("pawrly_fn__{namespace}__{name}")
}

/// Live connection handle for an **attached** function, so it shares its parent
/// source's rate-limiter / MCP session instead of opening a parallel one.
/// `None` for standalone/builtin functions (they build their own connection).
#[derive(Default, Clone, Debug)]
pub(crate) enum SourceHandle {
    #[default]
    None,
    Http(Arc<pawrly_sources_http::HttpSource>),
    Mcp(Arc<pawrly_sources_mcp::McpClientSession>),
}

/// Per-kind executor.
pub(crate) enum FunctionExecutor {
    Http(pawrly_sources_http::HttpFunctionExecutor),
    Mcp(pawrly_sources_mcp::McpFunctionExecutor),
    File(file::FileGlobExecutor),
}

impl FunctionExecutor {
    pub(crate) async fn invoke(
        &self,
        params: &BTreeMap<String, String>,
        limit: Option<usize>,
    ) -> datafusion::common::Result<RecordBatch> {
        match self {
            FunctionExecutor::Http(e) => e.invoke(params, limit).await,
            FunctionExecutor::Mcp(e) => e.invoke(params, limit).await,
            FunctionExecutor::File(e) => e.invoke(params, limit).await,
        }
    }
}

/// A fully-resolved, registered function: its definition, mangled UDTF name,
/// plan-time schema, executor, and (for attached functions) the parent source
/// name for teardown.
pub(crate) struct RegisteredFunction {
    pub def: FunctionDef,
    pub mangled: String,
    pub schema: SchemaRef,
    pub executor: FunctionExecutor,
    pub source: Option<String>,
}

/// `(namespace, name)` → function. Lives on the engine behind a `RwLock`, same
/// pattern as `sources`.
#[derive(Default)]
pub(crate) struct FunctionRegistry {
    by_name: HashMap<(String, String), Arc<RegisteredFunction>>,
}

impl FunctionRegistry {
    pub(crate) fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    pub(crate) fn contains(&self, namespace: &str, name: &str) -> bool {
        self.by_name
            .contains_key(&(namespace.to_string(), name.to_string()))
    }

    pub(crate) fn get(&self, namespace: &str, name: &str) -> Option<Arc<RegisteredFunction>> {
        self.by_name
            .get(&(namespace.to_string(), name.to_string()))
            .cloned()
    }

    pub(crate) fn insert(&mut self, f: Arc<RegisteredFunction>) {
        self.by_name
            .insert((f.def.namespace.clone(), f.def.name.clone()), f);
    }

    /// Drain the whole registry, returning every mangled UDTF name (for a full
    /// reload).
    pub(crate) fn drain_mangled(&mut self) -> Vec<String> {
        self.by_name
            .drain()
            .map(|(_, f)| f.mangled.clone())
            .collect()
    }

    /// Drop every function attached to `source`; returns their mangled UDTF
    /// names so the caller can `deregister_udtf` them.
    pub(crate) fn remove_by_source(&mut self, source: &str) -> Vec<String> {
        let keys: Vec<(String, String)> = self
            .by_name
            .iter()
            .filter(|(_, f)| f.source.as_deref() == Some(source))
            .map(|(k, _)| k.clone())
            .collect();
        keys.into_iter()
            .filter_map(|k| self.by_name.remove(&k).map(|f| f.mangled.clone()))
            .collect()
    }

    /// Sorted `ns.name` list, for the rewrite's unknown-function error.
    pub(crate) fn declared_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .by_name
            .keys()
            .map(|(ns, n)| format!("{ns}.{n}"))
            .collect();
        v.sort();
        v
    }

    /// All function infos, sorted, for `list_functions`.
    pub(crate) fn infos(&self) -> Vec<FunctionInfo> {
        let mut v: Vec<FunctionInfo> = self.by_name.values().map(|f| f.def.info()).collect();
        v.sort_by(|a, b| (&a.namespace, &a.name).cmp(&(&b.namespace, &b.name)));
        v
    }

    /// Full description for `describe_function`.
    pub(crate) fn describe(&self, namespace: &str, name: &str) -> Option<FunctionDescription> {
        self.get(namespace, name).map(|f| f.def.describe())
    }
}

/// Build a registered function. An attached function reuses the parent source's
/// `handle`; a standalone or builtin function (`handle = None`) builds its own
/// connection from its `connection` config (hence `async`).
pub(crate) async fn build_registered_function(
    def: FunctionDef,
    handle: SourceHandle,
    workspace_dir: &Path,
) -> Result<Arc<RegisteredFunction>, EngineError> {
    let (executor, schema) = build_executor(&def, handle, workspace_dir.to_path_buf()).await?;
    let mangled = mangle(&def.namespace, &def.name);
    let source = def.source.clone();
    Ok(Arc::new(RegisteredFunction {
        def,
        mangled,
        schema,
        executor,
        source,
    }))
}

async fn build_executor(
    def: &FunctionDef,
    handle: SourceHandle,
    workspace_dir: PathBuf,
) -> Result<(FunctionExecutor, SchemaRef), EngineError> {
    // Optional per-function pagination cap (`pagination.max_pages`).
    let max_pages = def
        .body
        .get("pagination")
        .and_then(|p| p.get("max_pages"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| u32::try_from(n).ok());

    match def.kind {
        FunctionKind::Http => {
            let source = match handle {
                SourceHandle::Http(s) => s,
                _ => pawrly_sources_http::build_http_source(&def.namespace, &def.connection)
                    .map_err(|e| EngineError::Internal(e.to_string()))?,
            };
            let exec = pawrly_sources_http::HttpFunctionExecutor::new(source, def, max_pages)
                .map_err(|e| EngineError::Internal(e.to_string()))?;
            let schema = exec.schema.clone();
            Ok((FunctionExecutor::Http(exec), schema))
        }
        FunctionKind::Mcp => {
            let session = match handle {
                SourceHandle::Mcp(s) => s,
                _ => pawrly_sources_mcp::build_mcp_session(&def.connection)
                    .await
                    .map_err(|e| EngineError::Internal(e.to_string()))?,
            };
            let exec = pawrly_sources_mcp::McpFunctionExecutor::new(session, def, max_pages)
                .map_err(|e| EngineError::Internal(e.to_string()))?;
            let schema = exec.schema();
            Ok((FunctionExecutor::Mcp(exec), schema))
        }
        FunctionKind::File => {
            let exec = file::FileGlobExecutor::new(def, workspace_dir)?;
            let schema = exec.schema();
            Ok((FunctionExecutor::File(exec), schema))
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// A registry holding only `file`-kind entries for the given `(ns, name)`
    /// pairs — enough for rewrite tests, which check names, not execution.
    pub(crate) fn registry_with(pairs: &[(&str, &str)]) -> FunctionRegistry {
        let mut reg = FunctionRegistry::default();
        for (ns, name) in pairs {
            let def = FunctionDef {
                namespace: (*ns).to_string(),
                name: (*name).to_string(),
                kind: FunctionKind::File,
                description: None,
                wiki: None,
                examples: vec![],
                args: vec![],
                returns: vec![],
                connection: serde_json::Value::Null,
                body: serde_json::json!({ "path": "*" }),
                source: None,
                builtin: true,
                cache: Default::default(),
                safety: None,
            };
            let exec = file::FileGlobExecutor::new(&def, PathBuf::from("."))
                .expect("file executor in test");
            let schema = exec.schema();
            reg.insert(Arc::new(RegisteredFunction {
                mangled: mangle(ns, name),
                def,
                schema,
                executor: FunctionExecutor::File(exec),
                source: None,
            }));
        }
        reg
    }
}
