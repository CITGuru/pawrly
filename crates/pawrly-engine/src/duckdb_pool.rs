//! In-process DuckDB connection pool used by `LocalEngine`.
//!
//! DuckDB is a sync FFI library, so every method on this pool wraps its work
//! in `tokio::task::spawn_blocking`. A single shared in-memory database is
//! held behind a `Mutex`; the `Semaphore` bounds how many in-flight callers
//! contend for it. DuckDB itself parallelizes work within a single query, so
//! one connection is sufficient for v1; M7 may revisit by holding multiple
//! connections cloned from the same underlying database if pool contention
//! becomes a measurable bottleneck.

use std::collections::HashSet;
use std::sync::Arc;

use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;
use datafusion::error::DataFusionError;
use datafusion::physical_plan::SendableRecordBatchStream;
use datafusion::physical_plan::stream::RecordBatchStreamAdapter;
use duckdb::Connection;
use parking_lot::Mutex;
use pawrly_core::EngineError;
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;

const BATCH_CHANNEL_DEPTH: usize = 2;

/// In-process DuckDB pool: one shared in-memory database, a bounded semaphore
/// for concurrency, and a memo of which extensions have been loaded so
/// `ensure_extension` is idempotent.
pub struct DuckDbPool {
    db: Arc<Mutex<Connection>>,
    semaphore: Arc<Semaphore>,
    extensions: Mutex<HashSet<String>>,
    offline: bool,
}

impl std::fmt::Debug for DuckDbPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DuckDbPool")
            .field("offline", &self.offline)
            .field("loaded_extensions", &self.extensions.lock().len())
            .field("permits", &self.semaphore.available_permits())
            .finish()
    }
}

impl DuckDbPool {
    /// Open a fresh in-memory database with `size` permits. Reads
    /// `PAWRLY_OFFLINE` from the environment to decide whether to skip
    /// `INSTALL` in [`Self::ensure_extension`].
    pub fn new(size: usize) -> Result<Self, EngineError> {
        Self::with_offline(size, env_offline())
    }

    /// Same as [`Self::new`] but with an explicit offline flag (for tests
    /// that should not depend on environment).
    pub fn with_offline(size: usize, offline: bool) -> Result<Self, EngineError> {
        let conn = Connection::open_in_memory()
            .map_err(|e| EngineError::Internal(format!("duckdb open_in_memory: {e}")))?;
        Ok(Self {
            db: Arc::new(Mutex::new(conn)),
            semaphore: Arc::new(Semaphore::new(size.max(1))),
            extensions: Mutex::new(HashSet::new()),
            offline,
        })
    }

    /// Whether this pool is in offline mode (no extension `INSTALL`).
    pub fn offline(&self) -> bool {
        self.offline
    }

    /// Install (when online) and load a DuckDB extension. Cached: a second
    /// call for the same extension is a no-op.
    pub async fn ensure_extension(&self, name: &str) -> Result<(), EngineError> {
        if self.extensions.lock().contains(name) {
            return Ok(());
        }
        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| EngineError::Internal(format!("duckdb semaphore: {e}")))?;
        let conn = self.db.clone();
        let offline = self.offline;
        let ext = name.to_string();
        tokio::task::spawn_blocking(move || -> Result<(), EngineError> {
            let _permit = permit;
            let guard = conn.lock();
            if !offline {
                let install_sql = format!("INSTALL {ext};");
                guard
                    .execute_batch(&install_sql)
                    .map_err(|e| EngineError::Internal(format!("INSTALL {ext}: {e}")))?;
            }
            let load_sql = format!("LOAD {ext};");
            guard
                .execute_batch(&load_sql)
                .map_err(|e| EngineError::Internal(format!("LOAD {ext}: {e}")))?;
            Ok(())
        })
        .await
        .map_err(|e| EngineError::Internal(format!("duckdb spawn_blocking: {e}")))??;
        self.extensions.lock().insert(name.to_string());
        Ok(())
    }

    /// Run one or more SQL statements that produce no result.
    pub async fn execute(&self, sql: &str) -> Result<(), EngineError> {
        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| EngineError::Internal(format!("duckdb semaphore: {e}")))?;
        let conn = self.db.clone();
        let sql = sql.to_string();
        tokio::task::spawn_blocking(move || -> Result<(), EngineError> {
            let _permit = permit;
            let guard = conn.lock();
            guard
                .execute_batch(&sql)
                .map_err(|e| EngineError::Internal(format!("duckdb execute: {e}")))?;
            Ok(())
        })
        .await
        .map_err(|e| EngineError::Internal(format!("duckdb spawn_blocking: {e}")))?
    }

    /// Run a SQL query and collect every batch into memory. Convenience for
    /// callers that want a small, fully-materialized result (catalog
    /// queries, smoke tests).
    pub async fn fetch_arrow(&self, sql: &str) -> Result<Vec<RecordBatch>, EngineError> {
        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| EngineError::Internal(format!("duckdb semaphore: {e}")))?;
        let conn = self.db.clone();
        let sql = sql.to_string();
        tokio::task::spawn_blocking(move || -> Result<Vec<RecordBatch>, EngineError> {
            let _permit = permit;
            let guard = conn.lock();
            let mut stmt = guard
                .prepare(&sql)
                .map_err(|e| EngineError::Internal(format!("duckdb prepare: {e}")))?;
            let arrow_iter = stmt
                .query_arrow([])
                .map_err(|e| EngineError::Internal(format!("duckdb query_arrow: {e}")))?;
            Ok(arrow_iter.collect())
        })
        .await
        .map_err(|e| EngineError::Internal(format!("duckdb spawn_blocking: {e}")))?
    }

    /// Run a SQL query and stream batches as DuckDB produces them. The
    /// returned stream is `Send`; the underlying `duckdb::Arrow` iterator is
    /// not, so it is pumped from a `spawn_blocking` task into a bounded
    /// `mpsc` channel that backpressures the producer.
    pub async fn fetch_arrow_stream(
        &self,
        sql: &str,
    ) -> Result<SendableRecordBatchStream, EngineError> {
        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| EngineError::Internal(format!("duckdb semaphore: {e}")))?;
        let (schema_tx, schema_rx) = oneshot::channel::<Result<SchemaRef, EngineError>>();
        let (batch_tx, batch_rx) =
            mpsc::channel::<Result<RecordBatch, DataFusionError>>(BATCH_CHANNEL_DEPTH);
        let conn = self.db.clone();
        let sql = sql.to_string();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let guard = conn.lock();
            let mut stmt = match guard.prepare(&sql) {
                Ok(s) => s,
                Err(e) => {
                    let _ =
                        schema_tx.send(Err(EngineError::Internal(format!("duckdb prepare: {e}"))));
                    return;
                }
            };
            let arrow_iter = match stmt.query_arrow([]) {
                Ok(it) => it,
                Err(e) => {
                    let _ = schema_tx.send(Err(EngineError::Internal(format!(
                        "duckdb query_arrow: {e}"
                    ))));
                    return;
                }
            };
            let schema = arrow_iter.get_schema();
            if schema_tx.send(Ok(schema)).is_err() {
                return;
            }
            for batch in arrow_iter {
                if batch_tx.blocking_send(Ok(batch)).is_err() {
                    break;
                }
            }
        });
        let schema = schema_rx
            .await
            .map_err(|e| EngineError::Internal(format!("duckdb schema rx: {e}")))??;
        let stream = ReceiverStream::new(batch_rx);
        Ok(Box::pin(RecordBatchStreamAdapter::new(schema, stream)) as SendableRecordBatchStream)
    }
}

fn env_offline() -> bool {
    matches!(
        std::env::var("PAWRLY_OFFLINE")
            .ok()
            .as_deref()
            .map(str::trim),
        Some("1") | Some("true") | Some("TRUE")
    )
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "tests"
    )]

    use super::*;
    use arrow_array::Int32Array;
    use arrow_schema::DataType;

    #[tokio::test]
    async fn pool_round_trips_arrow_query() {
        let pool = DuckDbPool::with_offline(2, true).expect("pool");
        let batches = pool
            .fetch_arrow("SELECT 1::INTEGER AS x")
            .await
            .expect("fetch_arrow");
        assert_eq!(batches.len(), 1, "expected exactly one batch");
        let batch = &batches[0];
        assert_eq!(batch.num_rows(), 1);
        assert_eq!(batch.num_columns(), 1);
        let field = batch.schema().field(0).clone();
        assert_eq!(field.name(), "x");
        assert_eq!(field.data_type(), &DataType::Int32);
        let col = batch
            .column(0)
            .as_any()
            .downcast_ref::<Int32Array>()
            .expect("Int32 array");
        assert_eq!(col.value(0), 1);
    }

    #[tokio::test]
    async fn pool_loads_extension_idempotently() {
        // `LOAD json` reads from `~/.duckdb/extensions/<linked-version>/...`,
        // so a pre-existing cache for a different DuckDB version makes the
        // load fail (POWA-122). Run the full INSTALL+LOAD path here so the
        // extension is fetched for whatever version libduckdb-sys is linked
        // against. If the host is offline and the cache is stale/missing,
        // skip rather than fail — this test exercises the in-process cache
        // memo, not network availability.
        let pool = DuckDbPool::with_offline(1, false).expect("pool");
        if let Err(e) = pool.ensure_extension("json").await {
            eprintln!(
                "skipping pool_loads_extension_idempotently: \
                 could not INSTALL+LOAD json (host offline and extension \
                 cache stale or missing for the linked DuckDB version): {e}"
            );
            return;
        }
        // Second call: should hit the in-memory memo and be a no-op.
        pool.ensure_extension("json").await.expect("second load");
        assert!(pool.extensions.lock().contains("json"));
    }
}
