//! Adapt DataFusion's `SendableRecordBatchStream` (which yields
//! `Result<RecordBatch, DataFusionError>`) into the trait's `QueryStream`
//! (which yields `Result<RecordBatch, EngineError>`), and instrument it with the
//! query lifecycle metrics.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::task::{Context, Poll};
use std::time::Instant;

use arrow_array::RecordBatch;
use chrono::Utc;
use datafusion::physical_plan::SendableRecordBatchStream;
use futures_util::Stream;
use futures_util::StreamExt as _;
use opentelemetry::KeyValue;
use parking_lot::Mutex;
use pawrly_core::activity::{ActivityRecord, Interface, Operation, Status};
use pawrly_core::{EngineError, QueryCompleted, QueryCompletion, QueryId, QueryStream};

use crate::activity::ActivitySink;

/// A per-query cancel flag: set by `EngineService::cancel` and polled
/// cooperatively by the result stream between batches.
pub type CancelFlag = Arc<AtomicBool>;

/// The in-flight query registry: maps a live query's [`QueryId`] to its
/// [`CancelFlag`]. Entries are inserted at query start and removed by the
/// [`QueryGuard`] when the result stream ends or is dropped.
pub type CancelRegistry = Arc<Mutex<HashMap<QueryId, CancelFlag>>>;

/// The static parts of an [`ActivityRecord`], captured when a query starts. The
/// dynamic parts (status, rows, duration) are filled by [`QueryGuard`] on drop.
pub struct ActivityContext {
    pub sink: ActivitySink,
    pub id: String,
    pub interface: Interface,
    pub principal: Option<String>,
    pub operation: Operation,
    /// Already redacted per policy.
    pub sql: Option<String>,
    pub param_keys: Vec<String>,
    pub trace_id: Option<String>,
}

pub fn adapt(inner: SendableRecordBatchStream) -> QueryStream {
    let mapped =
        inner.map(|item| item.map_err(|e| EngineError::Internal(format!("datafusion: {e}"))));
    Box::pin(mapped) as QueryStream
}

/// Tracks one in-flight query for metrics. Construction increments the active
/// gauge; `Drop` decrements it and records the terminal `pawrly.query.*`
/// instruments. The guard is moved into [`adapt_instrumented`] on success, so it
/// lives until the result stream completes or is dropped (covering early
/// cancellation); on an error before the stream is produced, it drops at the
/// call site and records `status = error`.
pub struct QueryGuard {
    active: Arc<AtomicI64>,
    start: Instant,
    rows: u64,
    /// Pessimistic until [`QueryGuard::mark_ok`] flips it on clean completion.
    status: &'static str,
    /// Stable error code, set by [`QueryGuard::mark_error`] on failure.
    error_code: Option<String>,
    /// Set when activity logging is on; emitted as a record on drop.
    activity: Option<ActivityContext>,
    cancel: Option<(QueryId, CancelRegistry)>,
    completion: Option<QueryCompletion>,
}

impl QueryGuard {
    /// Begin tracking: bump the active count (process-local atomic, read by
    /// `health()`, plus the OTel up/down counter).
    pub fn start(active: Arc<AtomicI64>) -> Self {
        active.fetch_add(1, Ordering::Relaxed);
        pawrly_telemetry::metrics::query_active().add(1, &[]);
        Self {
            active,
            start: Instant::now(),
            rows: 0,
            status: "error",
            error_code: None,
            activity: None,
            cancel: None,
            completion: None,
        }
    }

    /// Register this query in the cancel registry under `id`; the entry is
    /// removed when the guard drops (the result stream ended or was dropped).
    pub fn with_cancel(mut self, id: QueryId, registry: CancelRegistry) -> Self {
        self.cancel = Some((id, registry));
        self
    }

    /// Attach a write-once completion slot to fill with the terminal
    /// `rows_returned` / `truncated` / `elapsed` when the query ends.
    pub fn with_completion(mut self, slot: QueryCompletion) -> Self {
        self.completion = Some(slot);
        self
    }

    /// Attach an activity context so a record is emitted when the query
    /// finishes (or is dropped).
    pub fn with_activity(mut self, ctx: ActivityContext) -> Self {
        self.activity = Some(ctx);
        self
    }

    /// Set the recorded SQL on the attached activity context once it is known
    /// (e.g. after compiling a semantic query to SQL). No-op when activity
    /// logging is off.
    pub fn set_activity_sql(&mut self, sql: Option<String>) {
        if let Some(ctx) = self.activity.as_mut() {
            ctx.sql = sql;
        }
    }

    fn mark_ok(&mut self, rows: u64) {
        self.status = "ok";
        self.rows = rows;
    }

    /// Record the failure's stable code. `status` is already pessimistic, so
    /// this only captures the code (the same one surfaced over gRPC metadata).
    pub fn mark_error(&mut self, err: &EngineError) {
        self.error_code = Some(err.code().to_string());
    }
}

impl Drop for QueryGuard {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::Relaxed);
        pawrly_telemetry::metrics::query_active().add(-1, &[]);

        let status_attrs = [KeyValue::new("status", self.status)];
        let mut total_attrs = vec![KeyValue::new("status", self.status)];
        if let Some(code) = &self.error_code {
            total_attrs.push(KeyValue::new("error_code", code.clone()));
        }
        pawrly_telemetry::metrics::query_total().add(1, &total_attrs);
        pawrly_telemetry::metrics::query_duration()
            .record(self.start.elapsed().as_secs_f64() * 1000.0, &status_attrs);
        if self.status == "ok" {
            pawrly_telemetry::metrics::query_rows_returned().record(self.rows, &[]);
        }

        if let Some(ctx) = self.activity.take() {
            let ok = self.status == "ok";
            ctx.sink.emit(ActivityRecord {
                id: ctx.id,
                at: Utc::now(),
                interface: ctx.interface,
                principal: ctx.principal,
                operation: ctx.operation,
                sql: ctx.sql,
                param_keys: ctx.param_keys,
                status: if ok { Status::Ok } else { Status::Error },
                error_code: self.error_code.take(),
                duration_ms: self.start.elapsed().as_millis() as u64,
                rows_returned: ok.then_some(self.rows),
                bytes: None,
                trace_id: ctx.trace_id,
            });
        }

        if let Some(slot) = self.completion.take() {
            let _ = slot.set(QueryCompleted {
                rows_returned: self.rows,
                // Real truncation isn't enforced in the engine yet; false
                // matches the gRPC Completed frame.
                truncated: false,
                elapsed: self.start.elapsed(),
            });
        }
        if let Some((id, registry)) = self.cancel.take() {
            registry.lock().remove(&id);
        }
    }
}

/// Adapt a DataFusion stream and attach a [`QueryGuard`]: counts rows as they
/// flow and finalizes the metrics when the stream ends (or is dropped).
pub fn adapt_instrumented(
    inner: SendableRecordBatchStream,
    guard: QueryGuard,
    cancel: Option<CancelFlag>,
) -> QueryStream {
    Box::pin(InstrumentedStream {
        inner: adapt(inner),
        guard: Some(guard),
        rows: 0,
        cancel,
        cancel_emitted: false,
    })
}

struct InstrumentedStream {
    inner: QueryStream,
    /// `Some` until the terminal item is observed; taking it triggers the
    /// guard's `Drop` (and the final metric records) exactly once.
    guard: Option<QueryGuard>,
    rows: u64,
    cancel: Option<CancelFlag>,
    cancel_emitted: bool,
}

impl Stream for InstrumentedStream {
    type Item = Result<RecordBatch, EngineError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // `QueryStream` is `Pin<Box<..>>` (Unpin), and the other fields are
        // Unpin, so the whole struct is Unpin and `get_mut` is sound.
        let this = self.get_mut();

        // Cooperative cancellation: emit one `Cancelled`, then end (dropping the
        // guard finalizes metrics/completion and deregisters).
        if this.cancel_emitted {
            this.guard.take();
            return Poll::Ready(None);
        }
        if let Some(flag) = &this.cancel {
            if flag.load(Ordering::Relaxed) {
                this.cancel_emitted = true;
                if let Some(guard) = this.guard.as_mut() {
                    guard.mark_error(&EngineError::Cancelled);
                }
                return Poll::Ready(Some(Err(EngineError::Cancelled)));
            }
        }

        match this.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(batch))) => {
                this.rows += batch.num_rows() as u64;
                Poll::Ready(Some(Ok(batch)))
            }
            Poll::Ready(Some(Err(e))) => {
                if let Some(guard) = this.guard.as_mut() {
                    guard.mark_error(&e);
                }
                Poll::Ready(Some(Err(e)))
            }
            Poll::Ready(None) => {
                if let Some(mut guard) = this.guard.take() {
                    guard.mark_ok(this.rows);
                }
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}
