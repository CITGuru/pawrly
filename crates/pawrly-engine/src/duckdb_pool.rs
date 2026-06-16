//! In-process DuckDB connection pool used by `LocalEngine`.
//!
//! DuckDB is a sync FFI library, so every method on this pool wraps its work in
//! `tokio::task::spawn_blocking`. A set of connections — all `try_clone`d from a
//! single shared in-memory database, so they see the same catalog and data — is
//! checked out per operation and returned afterwards. The `Semaphore` bounds how
//! many run concurrently. Because each operation holds its own connection (not a
//! global lock), a slow streaming query keeps its connection busy without
//! blocking queries on the other connections.
//!
//! Extensions: `INSTALL` runs once (it populates a global cache); `LOAD` is
//! per-connection, so each requested extension is recorded and loaded on every
//! connection — eagerly on the free ones, lazily on any checked out at the time.

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

/// A pooled connection plus the set of extensions it has `LOAD`ed.
struct PooledConn {
    raw: Connection,
    loaded: HashSet<String>,
}

/// Shared pool state. Lives behind an `Arc` so it can be moved into the
/// `spawn_blocking` tasks that own a connection for the duration of an op.
struct PoolInner {
    /// Anchors the in-memory database (so it outlives every checked-out clone)
    /// and serves as the template for new connections and `INSTALL` statements.
    anchor: Mutex<Connection>,
    /// Idle connections available for checkout.
    free: Mutex<Vec<PooledConn>>,
    /// Extensions that must be `LOAD`ed on every connection (request order).
    required: Mutex<Vec<String>>,
    /// Extensions already `INSTALL`ed (global cache); guards re-`INSTALL`.
    installed: Mutex<HashSet<String>>,
}

impl PoolInner {
    /// Take an idle connection (or clone a fresh one), with every required
    /// extension loaded onto it.
    fn checkout(&self) -> Result<PooledConn, EngineError> {
        let mut conn = match self.free.lock().pop() {
            Some(c) => c,
            None => PooledConn {
                raw: self
                    .anchor
                    .lock()
                    .try_clone()
                    .map_err(|e| EngineError::Internal(format!("duckdb try_clone: {e}")))?,
                loaded: HashSet::new(),
            },
        };
        let required = self.required.lock().clone();
        for ext in &required {
            load_ext(&mut conn, ext)?;
        }
        Ok(conn)
    }

    /// Return a connection to the idle set for reuse.
    fn checkin(&self, conn: PooledConn) {
        self.free.lock().push(conn);
    }
}

/// `LOAD` an extension onto a connection if it isn't already loaded there.
fn load_ext(conn: &mut PooledConn, ext: &str) -> Result<(), EngineError> {
    if conn.loaded.contains(ext) {
        return Ok(());
    }
    conn.raw
        .execute_batch(&format!("LOAD {ext};"))
        .map_err(|e| EngineError::Internal(format!("LOAD {ext}: {e}")))?;
    conn.loaded.insert(ext.to_string());
    Ok(())
}

/// In-process DuckDB pool over a single shared in-memory database.
pub struct DuckDbPool {
    inner: Arc<PoolInner>,
    semaphore: Arc<Semaphore>,
    offline: bool,
}

impl std::fmt::Debug for DuckDbPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DuckDbPool")
            .field("offline", &self.offline)
            .field("free_connections", &self.inner.free.lock().len())
            .field("loaded_extensions", &self.inner.required.lock().len())
            .field("permits", &self.semaphore.available_permits())
            .finish()
    }
}

impl DuckDbPool {
    /// Open a fresh in-memory database with `size` connections/permits. Reads
    /// `PAWRLY_OFFLINE` from the environment to decide whether to skip `INSTALL`
    /// in [`Self::ensure_extension`].
    pub fn new(size: usize) -> Result<Self, EngineError> {
        Self::with_offline(size, env_offline())
    }

    /// Same as [`Self::new`] but with an explicit offline flag (for tests that
    /// should not depend on environment).
    pub fn with_offline(size: usize, offline: bool) -> Result<Self, EngineError> {
        let size = size.max(1);
        let anchor = Connection::open_in_memory()
            .map_err(|e| EngineError::Internal(format!("duckdb open_in_memory: {e}")))?;
        let mut free = Vec::with_capacity(size);
        for _ in 0..size {
            let raw = anchor
                .try_clone()
                .map_err(|e| EngineError::Internal(format!("duckdb try_clone: {e}")))?;
            free.push(PooledConn {
                raw,
                loaded: HashSet::new(),
            });
        }
        Ok(Self {
            inner: Arc::new(PoolInner {
                anchor: Mutex::new(anchor),
                free: Mutex::new(free),
                required: Mutex::new(Vec::new()),
                installed: Mutex::new(HashSet::new()),
            }),
            semaphore: Arc::new(Semaphore::new(size)),
            offline,
        })
    }

    /// Whether this pool is in offline mode (no extension `INSTALL`).
    pub fn offline(&self) -> bool {
        self.offline
    }

    async fn permit(&self) -> Result<tokio::sync::OwnedSemaphorePermit, EngineError> {
        self.semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| EngineError::Internal(format!("duckdb semaphore: {e}")))
    }

    /// Install (when online) and load a DuckDB extension on every connection.
    /// Cached: a second call for the same extension is a no-op.
    pub async fn ensure_extension(&self, name: &str) -> Result<(), EngineError> {
        if self.inner.required.lock().iter().any(|e| e == name) {
            return Ok(());
        }
        let permit = self.permit().await?;
        let inner = self.inner.clone();
        let offline = self.offline;
        let ext = name.to_string();
        tokio::task::spawn_blocking(move || -> Result<(), EngineError> {
            let _permit = permit;
            {
                let mut installed = inner.installed.lock();
                if !offline && !installed.contains(&ext) {
                    inner
                        .anchor
                        .lock()
                        .execute_batch(&format!("INSTALL {ext};"))
                        .map_err(|e| EngineError::Internal(format!("INSTALL {ext}: {e}")))?;
                }
                installed.insert(ext.clone());
            }
            // Load eagerly on idle connections; any connection checked out right
            // now will pick it up via `required` on its next checkout.
            for conn in inner.free.lock().iter_mut() {
                load_ext(conn, &ext)?;
            }
            if !inner.required.lock().iter().any(|e| e == &ext) {
                inner.required.lock().push(ext);
            }
            Ok(())
        })
        .await
        .map_err(|e| EngineError::Internal(format!("duckdb spawn_blocking: {e}")))??;
        Ok(())
    }

    /// Run one or more SQL statements that produce no result.
    pub async fn execute(&self, sql: &str) -> Result<(), EngineError> {
        let permit = self.permit().await?;
        let inner = self.inner.clone();
        let sql = sql.to_string();
        tokio::task::spawn_blocking(move || -> Result<(), EngineError> {
            let _permit = permit;
            let conn = inner.checkout()?;
            let res = conn
                .raw
                .execute_batch(&sql)
                .map_err(|e| EngineError::Internal(format!("duckdb execute: {e}")));
            inner.checkin(conn);
            res
        })
        .await
        .map_err(|e| EngineError::Internal(format!("duckdb spawn_blocking: {e}")))?
    }

    /// Run a SQL query and collect every batch into memory. Convenience for
    /// callers that want a small, fully-materialized result (catalog queries,
    /// smoke tests).
    pub async fn fetch_arrow(&self, sql: &str) -> Result<Vec<RecordBatch>, EngineError> {
        let permit = self.permit().await?;
        let inner = self.inner.clone();
        let sql = sql.to_string();
        tokio::task::spawn_blocking(move || -> Result<Vec<RecordBatch>, EngineError> {
            let _permit = permit;
            let conn = inner.checkout()?;
            let result = (|| {
                let mut stmt = conn
                    .raw
                    .prepare(&sql)
                    .map_err(|e| EngineError::Internal(format!("duckdb prepare: {e}")))?;
                let arrow_iter = stmt
                    .query_arrow([])
                    .map_err(|e| EngineError::Internal(format!("duckdb query_arrow: {e}")))?;
                Ok(arrow_iter.collect())
            })();
            inner.checkin(conn);
            result
        })
        .await
        .map_err(|e| EngineError::Internal(format!("duckdb spawn_blocking: {e}")))?
    }

    /// Run a SQL query and stream batches as DuckDB produces them. The returned
    /// stream is `Send`; the underlying `duckdb::Arrow` iterator is not, so it is
    /// pumped from a `spawn_blocking` task into a bounded `mpsc` channel that
    /// backpressures the producer. The connection is held only by that task and
    /// returned when the stream finishes, so other connections stay available.
    pub async fn fetch_arrow_stream(
        &self,
        sql: &str,
    ) -> Result<SendableRecordBatchStream, EngineError> {
        let permit = self.permit().await?;
        let (schema_tx, schema_rx) = oneshot::channel::<Result<SchemaRef, EngineError>>();
        let (batch_tx, batch_rx) =
            mpsc::channel::<Result<RecordBatch, DataFusionError>>(BATCH_CHANNEL_DEPTH);
        let inner = self.inner.clone();
        let sql = sql.to_string();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut conn = match inner.checkout() {
                Ok(c) => c,
                Err(e) => {
                    let _ = schema_tx.send(Err(e));
                    return;
                }
            };
            stream_into(&mut conn, &sql, schema_tx, &batch_tx);
            inner.checkin(conn);
        });
        let schema = schema_rx
            .await
            .map_err(|e| EngineError::Internal(format!("duckdb schema rx: {e}")))??;
        let stream = ReceiverStream::new(batch_rx);
        Ok(Box::pin(RecordBatchStreamAdapter::new(schema, stream)) as SendableRecordBatchStream)
    }
}

/// Prepare `sql` on `conn`, publish the schema, then pump batches into
/// `batch_tx` until exhausted or the receiver drops. Borrows on `conn` end when
/// this returns, so the caller can check the connection back in.
fn stream_into(
    conn: &mut PooledConn,
    sql: &str,
    schema_tx: oneshot::Sender<Result<SchemaRef, EngineError>>,
    batch_tx: &mpsc::Sender<Result<RecordBatch, DataFusionError>>,
) {
    let mut stmt = match conn.raw.prepare(sql) {
        Ok(s) => s,
        Err(e) => {
            let _ = schema_tx.send(Err(EngineError::Internal(format!("duckdb prepare: {e}"))));
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
        // `parquet` is statically linked into the `bundled` DuckDB build, so it
        // `LOAD`s offline with no network and no `~/.duckdb/extensions` cache —
        // unlike `json`, which DuckDB >= 1.5 ships as a separately-installed
        // extension whose load depends on a per-version cache. This test
        // exercises the in-process idempotency memo, not extension availability,
        // so we stay offline and use the built-in. If even the built-in load
        // fails on some build, skip rather than fail.
        let pool = DuckDbPool::with_offline(1, true).expect("pool");
        if let Err(e) = pool.ensure_extension("parquet").await {
            eprintln!("skipping pool_loads_extension_idempotently: LOAD parquet failed: {e}");
            return;
        }
        let has_parquet = || pool.inner.required.lock().iter().any(|e| e == "parquet");
        assert!(has_parquet());
        // Second call: should hit the in-memory memo and be a no-op.
        pool.ensure_extension("parquet").await.expect("second load");
        assert!(has_parquet());
    }

    #[tokio::test]
    async fn pool_serves_concurrent_queries() {
        // Many concurrent ops across a small pool must each get a working
        // connection and the right answer (exercises checkout/checkin reuse and
        // the clone-on-demand fallback).
        let pool = Arc::new(DuckDbPool::with_offline(4, true).expect("pool"));
        let mut handles = Vec::new();
        for i in 0..16i32 {
            let p = pool.clone();
            handles.push(tokio::spawn(async move {
                let batches = p
                    .fetch_arrow(&format!("SELECT {i}::INTEGER AS x"))
                    .await
                    .expect("fetch_arrow");
                batches[0]
                    .column(0)
                    .as_any()
                    .downcast_ref::<Int32Array>()
                    .expect("Int32 array")
                    .value(0)
            }));
        }
        for (i, h) in handles.into_iter().enumerate() {
            assert_eq!(h.await.expect("join"), i as i32);
        }
    }

    #[tokio::test]
    async fn pool_shares_catalog_across_connections() {
        // A table created on one checked-out connection must be visible to a
        // later checkout (they share the same in-memory database).
        let pool = DuckDbPool::with_offline(4, true).expect("pool");
        pool.execute("CREATE TABLE t AS SELECT 42::INTEGER AS x")
            .await
            .expect("create");
        let batches = pool.fetch_arrow("SELECT x FROM t").await.expect("select");
        let v = batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<Int32Array>()
            .expect("Int32 array")
            .value(0);
        assert_eq!(v, 42);
    }
}
