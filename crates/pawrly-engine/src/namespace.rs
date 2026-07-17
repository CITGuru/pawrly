//! Materialize namespaces and the read-only catalog that makes cached
//! snapshots and materialized tables SQL-addressable, without changing the
//! transparent read-through path.
//!
//! Each namespace is one `CacheManager` store under the shared storage root.
//! The workspace's own namespace is registered eagerly; any other —
//! `materialize(…, namespace)` targets, other workspaces, stores written by
//! another process — resolves on demand through [`DynamicNamespaceCatalogs`]:
//!
//! ```sql
//! SELECT * FROM github.issues;          -- live read-through wrapper
//! SELECT * FROM untwine.github.issues;  -- the cached snapshot, read directly
//! SELECT * FROM sess_a.materialized.t;  -- a per-call materialize namespace
//! ```
//!
//! Within a catalog, schemas are the distinct `source` values in the manifest,
//! tables are the entries under each, and a lookup returns a
//! [`ParquetSnapshotProvider`] over the entry's file. The manifest is read on
//! every call, so the catalog always reflects what is on disk with no coupling
//! to the write path. Reads are expiry-agnostic — every entry whose file is
//! present is exposed, regardless of `expires_at` (freshness applies only to
//! live reads).

use std::any::Any;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use datafusion::catalog::{
    CatalogProvider, CatalogProviderList, MemoryCatalogProviderList, SchemaProvider,
};
use datafusion::common::DataFusionError;
use datafusion::datasource::TableProvider;
use parking_lot::RwLock;
use pawrly_core::EngineError;

use crate::cache::{CacheManager, ParquetSnapshotProvider};

/// A caller-supplied namespace must already be a clean segment (rejected, not
/// sanitized like the config namespace, so the SQL address is exactly what the
/// caller passed) and must not shadow a reserved catalog/schema name.
pub(crate) fn validate_namespace(ns: &str) -> Result<(), EngineError> {
    if !ns.chars().any(|c| c.is_ascii_alphanumeric()) || crate::local::sanitize_segment(ns) != ns {
        return Err(EngineError::Internal(format!(
            "invalid namespace `{ns}`: use alphanumerics, `_`, `-`, or `.`, \
             with at least one alphanumeric"
        )));
    }
    let reserved = [
        crate::local::PAWRLY_CATALOG,
        pawrly_core::MATERIALIZED_SCHEMA,
        pawrly_core::SYSTEM_SCHEMA,
        "information_schema",
    ];
    if reserved.contains(&ns) {
        return Err(EngineError::Internal(format!(
            "namespace `{ns}` is reserved"
        )));
    }
    Ok(())
}

/// Resolves per-call materialize namespaces to their [`CacheManager`]s: the
/// default is pre-seeded; any other maps to `<storage_root>/<ns>`, opened
/// lazily and memoized so each namespace's manifest state stays a
/// process-wide singleton.
#[derive(Debug)]
pub(crate) struct NamespaceRegistry {
    storage_root: PathBuf,
    default_ns: String,
    default_cache: Arc<CacheManager>,
    extra: RwLock<HashMap<String, Arc<CacheManager>>>,
}

impl NamespaceRegistry {
    pub(crate) fn new(
        storage_root: PathBuf,
        default_ns: String,
        default_cache: Arc<CacheManager>,
    ) -> Self {
        Self {
            storage_root,
            default_ns,
            default_cache,
            extra: RwLock::new(HashMap::new()),
        }
    }

    /// The manager backing a write; creates the store on first use.
    pub(crate) fn for_write(&self, ns: Option<&str>) -> Result<Arc<CacheManager>, EngineError> {
        match self.normalize(ns)? {
            None => Ok(self.default_cache.clone()),
            Some(ns) => self.open_or_create(ns),
        }
    }

    /// Like [`Self::for_write`] but never creates: an unknown namespace is
    /// `Ok(None)`, so a failed `drop`/`list` can't leave an empty store behind.
    pub(crate) fn for_read(
        &self,
        ns: Option<&str>,
    ) -> Result<Option<Arc<CacheManager>>, EngineError> {
        match self.normalize(ns)? {
            None => Ok(Some(self.default_cache.clone())),
            Some(ns) => self.open_existing(ns),
        }
    }

    /// Catalog-lookup fallback. Arbitrary catalog names land here, so the
    /// validity check must precede the filesystem join — it is what keeps a
    /// hostile name (`"../x"`) out of the storage root.
    pub(crate) fn lookup(&self, ns: &str) -> Option<Arc<CacheManager>> {
        if ns == self.default_ns {
            return Some(self.default_cache.clone());
        }
        validate_namespace(ns).ok()?;
        self.open_existing(ns).ok().flatten()
    }

    /// Namespaces for catalog enumeration. Skips the default namespace — the
    /// session's catalog list already carries it via its eager registration.
    pub(crate) fn known_namespaces(&self) -> Vec<String> {
        let mut names: Vec<String> = self.extra.read().keys().cloned().collect();
        if let Ok(rd) = std::fs::read_dir(&self.storage_root) {
            for entry in rd.flatten() {
                let Ok(name) = entry.file_name().into_string() else {
                    continue;
                };
                if name == self.default_ns
                    || names.contains(&name)
                    || validate_namespace(&name).is_err()
                {
                    continue;
                }
                if entry.path().join("manifest.json").is_file() {
                    names.push(name);
                }
            }
        }
        names
    }

    /// `None`/empty/the default namespace ⇒ `None` (the default manager).
    fn normalize<'a>(&self, ns: Option<&'a str>) -> Result<Option<&'a str>, EngineError> {
        match ns {
            None | Some("") => Ok(None),
            Some(ns) if ns == self.default_ns => Ok(None),
            Some(ns) => {
                validate_namespace(ns)?;
                Ok(Some(ns))
            }
        }
    }

    fn open_or_create(&self, ns: &str) -> Result<Arc<CacheManager>, EngineError> {
        if let Some(c) = self.extra.read().get(ns) {
            return Ok(c.clone());
        }
        let mut extra = self.extra.write();
        if let Some(c) = extra.get(ns) {
            return Ok(c.clone());
        }
        let cache = Arc::new(
            CacheManager::new(self.storage_root.join(ns))
                .map_err(|e| EngineError::Internal(format!("namespace `{ns}` cache init: {e}")))?,
        );
        extra.insert(ns.to_string(), cache.clone());
        Ok(cache)
    }

    /// Returns `false` if the namespace never existed; the default workspace
    /// namespace is refused — dropping it would take the live cache with it.
    pub(crate) fn remove(&self, ns: &str) -> Result<bool, EngineError> {
        if ns.is_empty() || ns == self.default_ns {
            return Err(EngineError::Internal(
                "cannot drop the default workspace namespace".to_string(),
            ));
        }
        validate_namespace(ns)?;
        let existed = self.extra.write().remove(ns).is_some();
        let dir = self.storage_root.join(ns);
        if dir.join("manifest.json").is_file() {
            std::fs::remove_dir_all(&dir)
                .map_err(|e| EngineError::Internal(format!("drop namespace `{ns}`: {e}")))?;
            return Ok(true);
        }
        Ok(existed)
    }

    /// A namespace "exists" once its manager is memoized or its manifest is on
    /// disk; a bare directory does not count.
    fn open_existing(&self, ns: &str) -> Result<Option<Arc<CacheManager>>, EngineError> {
        if self.extra.read().contains_key(ns)
            || self.storage_root.join(ns).join("manifest.json").is_file()
        {
            return self.open_or_create(ns).map(Some);
        }
        Ok(None)
    }
}

/// A `CatalogProviderList` that falls back from explicit registrations to any
/// namespace present under the storage root — what keeps
/// `<ns>.materialized.<name>` queryable across engine restarts and processes.
#[derive(Debug)]
pub(crate) struct DynamicNamespaceCatalogs {
    inner: MemoryCatalogProviderList,
    registry: Arc<NamespaceRegistry>,
}

impl DynamicNamespaceCatalogs {
    pub(crate) fn new(registry: Arc<NamespaceRegistry>) -> Self {
        Self {
            inner: MemoryCatalogProviderList::new(),
            registry,
        }
    }
}

impl CatalogProviderList for DynamicNamespaceCatalogs {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn register_catalog(
        &self,
        name: String,
        catalog: Arc<dyn CatalogProvider>,
    ) -> Option<Arc<dyn CatalogProvider>> {
        self.inner.register_catalog(name, catalog)
    }

    fn catalog_names(&self) -> Vec<String> {
        let mut names = self.inner.catalog_names();
        for ns in self.registry.known_namespaces() {
            if !names.contains(&ns) {
                names.push(ns);
            }
        }
        names
    }

    fn catalog(&self, name: &str) -> Option<Arc<dyn CatalogProvider>> {
        if let Some(c) = self.inner.catalog(name) {
            return Some(c);
        }
        let cache = self.registry.lookup(name)?;
        Some(Arc::new(NamespaceCatalogProvider::new(cache)))
    }
}

/// A `CatalogProvider` whose schemas are the manifest's `source` values and
/// whose tables are cached snapshots read directly from Parquet.
#[derive(Debug)]
pub struct NamespaceCatalogProvider {
    cache: Arc<CacheManager>,
}

impl NamespaceCatalogProvider {
    pub fn new(cache: Arc<CacheManager>) -> Self {
        Self { cache }
    }
}

impl CatalogProvider for NamespaceCatalogProvider {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn schema_names(&self) -> Vec<String> {
        self.cache.namespace_sources()
    }

    fn schema(&self, name: &str) -> Option<Arc<dyn SchemaProvider>> {
        // A schema exists exactly when the manifest has at least one
        // file-present entry for that source. Reading the manifest per call
        // keeps the catalog in lock-step with whatever the cache has written.
        if self.cache.namespace_sources().iter().any(|s| s == name) {
            Some(Arc::new(NamespaceSchemaProvider {
                cache: self.cache.clone(),
                source: name.to_string(),
            }))
        } else {
            None
        }
    }
}

/// A `SchemaProvider` over one `source`'s cached tables, for registering that
/// schema directly in the default catalog — used so `materialized.<name>`
/// resolves without the namespace prefix.
pub(crate) fn schema_provider_for(
    cache: Arc<CacheManager>,
    source: &str,
) -> Arc<dyn SchemaProvider> {
    Arc::new(NamespaceSchemaProvider {
        cache,
        source: source.to_string(),
    })
}

/// One source's worth of cached tables, resolved live from the manifest.
#[derive(Debug)]
struct NamespaceSchemaProvider {
    cache: Arc<CacheManager>,
    source: String,
}

#[async_trait]
impl SchemaProvider for NamespaceSchemaProvider {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn table_names(&self) -> Vec<String> {
        self.cache.namespace_tables(&self.source)
    }

    async fn table(&self, name: &str) -> Result<Option<Arc<dyn TableProvider>>, DataFusionError> {
        let Some(path) = self.cache.namespace_path(&self.source, name) else {
            return Ok(None);
        };
        let provider = ParquetSnapshotProvider::open(path)
            .map_err(|e| DataFusionError::External(Box::new(e)))?;
        Ok(Some(Arc::new(provider)))
    }

    fn table_exist(&self, name: &str) -> bool {
        self.cache.namespace_path(&self.source, name).is_some()
    }
}
