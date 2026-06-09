//! Read-only catalog that makes cached snapshots and materialized tables
//! SQL-addressable, without changing the transparent read-through path.
//!
//! Registered under the workspace namespace string, so the same data is also
//! reachable directly:
//!
//! ```sql
//! SELECT * FROM github.issues;          -- live read-through wrapper
//! SELECT * FROM untwine.github.issues;  -- the cached snapshot, read directly
//! ```
//!
//! Schemas are the distinct `source` values in the manifest, tables are the
//! entries under each, and a lookup returns a [`ParquetSnapshotProvider`] over
//! the entry's file. The manifest is read on every call, so the catalog always
//! reflects what is on disk with no coupling to the write path. Reads are
//! expiry-agnostic — every entry whose file is present is exposed, regardless of
//! `expires_at` (freshness applies only to live reads).

use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use datafusion::catalog::{CatalogProvider, SchemaProvider};
use datafusion::common::DataFusionError;
use datafusion::datasource::TableProvider;

use crate::cache::{CacheManager, ParquetSnapshotProvider};

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
