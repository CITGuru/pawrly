//! Activity-log plumbing inside the engine: the fire-and-forget sink that feeds
//! [`ActivityRecorder`]s, and the `tracing`-event recorder (sink 1).
//!
//! Recording is off the hot path: [`ActivitySink::emit`] does a non-blocking
//! `try_send` onto a bounded channel and, when full, drops the record and bumps
//! `pawrly.activity.dropped` rather than ever blocking a query (§6.2). A
//! background task drains the channel into the configured recorder.

use std::sync::Arc;

use async_trait::async_trait;
use pawrly_core::activity::{ActivityRecord, ActivityRecorder};
use tokio::sync::mpsc;

/// Non-blocking entry point for activity records. Cloneable and cheap; a
/// [`disabled`](ActivitySink::disabled) sink discards everything.
#[derive(Clone)]
pub struct ActivitySink {
    tx: Option<mpsc::Sender<ActivityRecord>>,
}

impl ActivitySink {
    /// A sink that records nothing — the engine default when activity logging is
    /// off.
    pub fn disabled() -> Self {
        Self { tx: None }
    }

    /// Spawn a background drain that forwards records to `recorder`. `capacity`
    /// bounds the in-flight queue; excess records are dropped (and counted)
    /// rather than blocking the caller.
    pub fn spawn(recorder: Arc<dyn ActivityRecorder>, capacity: usize) -> Self {
        let (tx, mut rx) = mpsc::channel::<ActivityRecord>(capacity.max(1));
        tokio::spawn(async move {
            while let Some(rec) = rx.recv().await {
                recorder.record(rec).await;
            }
        });
        Self { tx: Some(tx) }
    }

    /// Whether records are actually recorded (a spawned drain exists).
    pub fn is_enabled(&self) -> bool {
        self.tx.is_some()
    }

    /// Hand off a record without blocking. Drops and counts it if the queue is
    /// full or the drain has stopped.
    pub fn emit(&self, record: ActivityRecord) {
        if let Some(tx) = &self.tx
            && tx.try_send(record).is_err()
        {
            pawrly_telemetry::metrics::activity_dropped().add(1, &[]);
        }
    }
}

/// Sink 1: emit each record as a structured `tracing` event on the
/// `pawrly.activity` target. With the JSON fmt layer this is line-delimited
/// JSON; with the OTel logs bridge it becomes an OTel log record (§6.5).
pub struct TracingRecorder;

#[async_trait]
impl ActivityRecorder for TracingRecorder {
    async fn record(&self, rec: ActivityRecord) {
        tracing::info!(
            target: "pawrly.activity",
            id = %rec.id,
            at = %rec.at.to_rfc3339(),
            interface = rec.interface.as_str(),
            operation = rec.operation.as_str(),
            status = rec.status.as_str(),
            duration_ms = rec.duration_ms,
            rows_returned = rec.rows_returned,
            bytes = rec.bytes,
            error_code = rec.error_code.as_deref(),
            principal = rec.principal.as_deref(),
            trace_id = rec.trace_id.as_deref(),
            param_keys = ?rec.param_keys,
            sql = rec.sql.as_deref(),
            "activity"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use chrono::Utc;
    use pawrly_core::activity::{Interface, Operation, Status};

    use super::*;

    struct Capturing(Arc<Mutex<Vec<ActivityRecord>>>);

    #[async_trait]
    impl ActivityRecorder for Capturing {
        async fn record(&self, rec: ActivityRecord) {
            self.0.lock().unwrap().push(rec);
        }
    }

    fn sample() -> ActivityRecord {
        ActivityRecord {
            id: "q1".into(),
            at: Utc::now(),
            interface: Interface::Cli,
            principal: None,
            operation: Operation::Query,
            sql: Some("SELECT $REDACTED".into()),
            param_keys: vec![],
            status: Status::Ok,
            error_code: None,
            duration_ms: 1,
            rows_returned: Some(1),
            bytes: None,
            trace_id: None,
        }
    }

    #[tokio::test]
    async fn spawned_sink_drains_to_recorder() {
        let store = Arc::new(Mutex::new(Vec::new()));
        let sink = ActivitySink::spawn(Arc::new(Capturing(store.clone())), 8);
        assert!(sink.is_enabled());
        sink.emit(sample());
        // Yield until the background drain has the record.
        for _ in 0..50 {
            if store.lock().unwrap().len() == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(store.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn disabled_sink_is_noop() {
        let sink = ActivitySink::disabled();
        assert!(!sink.is_enabled());
        sink.emit(sample()); // must not panic
    }
}
