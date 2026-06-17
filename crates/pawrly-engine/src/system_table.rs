//! The `system.activity` table — sink 2 of the activity log.
//!
//! An [`ActivityStore`] keeps a bounded in-memory ring of recent records and is
//! itself an [`ActivityRecorder`], so the activity sink feeds it. The
//! [`ActivityTableProvider`] exposes the ring as a DataFusion table, letting
//! operators query their own activity with SQL.

use std::any::Any;
use std::collections::VecDeque;
use std::sync::Arc;

use arrow_array::builder::{ListBuilder, StringBuilder, UInt64Builder};
use arrow_array::{ArrayRef, RecordBatch, TimestampMicrosecondArray};
use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};
use async_trait::async_trait;
use datafusion::catalog::Session;
use datafusion::common::DataFusionError;
use datafusion::datasource::memory::MemorySourceConfig;
use datafusion::datasource::{TableProvider, TableType};
use datafusion::logical_expr::Expr;
use datafusion::physical_plan::ExecutionPlan;
use parking_lot::Mutex;
use pawrly_core::activity::{ActivityRecord, ActivityRecorder};

/// Arrow schema of `system.activity`. One column per [`ActivityRecord`] field.
/// Committed and extended additively only.
pub fn activity_schema() -> SchemaRef {
    let utf8 = |name: &str, nullable: bool| Field::new(name, DataType::Utf8, nullable);
    let u64 = |name: &str, nullable: bool| Field::new(name, DataType::UInt64, nullable);
    Arc::new(Schema::new(vec![
        utf8("id", false),
        Field::new(
            "at",
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            false,
        ),
        utf8("interface", false),
        utf8("principal", true),
        utf8("operation", false),
        utf8("sql", true),
        Field::new(
            "param_keys",
            // Item nullability matches arrow's `ListBuilder<StringBuilder>` output.
            DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
            false,
        ),
        utf8("status", false),
        utf8("error_code", true),
        u64("duration_ms", false),
        u64("rows_returned", true),
        u64("bytes", true),
        utf8("trace_id", true),
    ]))
}

/// Convert a slice of records into one Arrow batch matching [`activity_schema`].
fn records_to_batch(records: &[ActivityRecord], schema: &SchemaRef) -> Result<RecordBatch, DataFusionError> {
    let mut id = StringBuilder::new();
    let mut at = Vec::<i64>::with_capacity(records.len());
    let mut interface = StringBuilder::new();
    let mut principal = StringBuilder::new();
    let mut operation = StringBuilder::new();
    let mut sql = StringBuilder::new();
    let mut param_keys = ListBuilder::new(StringBuilder::new());
    let mut status = StringBuilder::new();
    let mut error_code = StringBuilder::new();
    let mut duration_ms = UInt64Builder::new();
    let mut rows_returned = UInt64Builder::new();
    let mut bytes = UInt64Builder::new();
    let mut trace_id = StringBuilder::new();

    for rec in records {
        id.append_value(&rec.id);
        at.push(rec.at.timestamp_micros());
        interface.append_value(rec.interface.as_str());
        principal.append_option(rec.principal.as_deref());
        operation.append_value(rec.operation.as_str());
        sql.append_option(rec.sql.as_deref());
        for key in &rec.param_keys {
            param_keys.values().append_value(key);
        }
        param_keys.append(true);
        status.append_value(rec.status.as_str());
        error_code.append_option(rec.error_code.as_deref());
        duration_ms.append_value(rec.duration_ms);
        rows_returned.append_option(rec.rows_returned);
        bytes.append_option(rec.bytes);
        trace_id.append_option(rec.trace_id.as_deref());
    }

    let at = TimestampMicrosecondArray::from(at).with_timezone("UTC");
    let columns: Vec<ArrayRef> = vec![
        Arc::new(id.finish()),
        Arc::new(at),
        Arc::new(interface.finish()),
        Arc::new(principal.finish()),
        Arc::new(operation.finish()),
        Arc::new(sql.finish()),
        Arc::new(param_keys.finish()),
        Arc::new(status.finish()),
        Arc::new(error_code.finish()),
        Arc::new(duration_ms.finish()),
        Arc::new(rows_returned.finish()),
        Arc::new(bytes.finish()),
        Arc::new(trace_id.finish()),
    ];
    RecordBatch::try_new(schema.clone(), columns)
        .map_err(|e| DataFusionError::ArrowError(Box::new(e), None))
}

/// Bounded in-memory ring of recent activity records. Cloneable handle over
/// shared state, so the recorder and the table provider observe the same ring.
#[derive(Clone, Debug)]
pub struct ActivityStore {
    ring: Arc<Mutex<VecDeque<ActivityRecord>>>,
    capacity: usize,
}

impl ActivityStore {
    /// A ring holding at most `capacity` records (at least one).
    pub fn new(capacity: usize) -> Self {
        Self {
            ring: Arc::new(Mutex::new(VecDeque::new())),
            capacity: capacity.max(1),
        }
    }

    fn push(&self, rec: ActivityRecord) {
        let mut ring = self.ring.lock();
        if ring.len() == self.capacity {
            ring.pop_front();
        }
        ring.push_back(rec);
    }

    fn snapshot(&self) -> Vec<ActivityRecord> {
        self.ring.lock().iter().cloned().collect()
    }
}

#[async_trait]
impl ActivityRecorder for ActivityStore {
    async fn record(&self, rec: ActivityRecord) {
        self.push(rec);
    }
}

/// DataFusion table backed by an [`ActivityStore`]. Each scan snapshots the ring
/// into a single batch, so a query sees a consistent point-in-time view.
#[derive(Debug)]
pub struct ActivityTableProvider {
    store: ActivityStore,
    schema: SchemaRef,
}

impl ActivityTableProvider {
    pub fn new(store: ActivityStore) -> Self {
        Self {
            store,
            schema: activity_schema(),
        }
    }
}

#[async_trait]
impl TableProvider for ActivityTableProvider {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    async fn scan(
        &self,
        _state: &dyn Session,
        projection: Option<&Vec<usize>>,
        _filters: &[Expr],
        _limit: Option<usize>,
    ) -> datafusion::common::Result<Arc<dyn ExecutionPlan>> {
        let batch = records_to_batch(&self.store.snapshot(), &self.schema)?;
        let exec =
            MemorySourceConfig::try_new_exec(&[vec![batch]], self.schema.clone(), projection.cloned())
                .map_err(|e| DataFusionError::Plan(e.to_string()))?;
        Ok(exec)
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use pawrly_core::activity::{Interface, Operation, Status};

    use super::*;

    fn rec(id: &str) -> ActivityRecord {
        ActivityRecord {
            id: id.into(),
            at: Utc::now(),
            interface: Interface::Cli,
            principal: None,
            operation: Operation::Query,
            sql: Some("SELECT $REDACTED".into()),
            param_keys: vec!["k".into()],
            status: Status::Ok,
            error_code: None,
            duration_ms: 3,
            rows_returned: Some(2),
            bytes: None,
            trace_id: None,
        }
    }

    #[test]
    fn ring_evicts_oldest_past_capacity() {
        let store = ActivityStore::new(2);
        store.push(rec("a"));
        store.push(rec("b"));
        store.push(rec("c"));
        let ids: Vec<_> = store.snapshot().into_iter().map(|r| r.id).collect();
        assert_eq!(ids, vec!["b", "c"]);
    }

    #[test]
    fn batch_matches_schema_and_row_count() {
        let schema = activity_schema();
        let batch = records_to_batch(&[rec("a"), rec("b")], &schema).unwrap();
        assert_eq!(batch.num_rows(), 2);
        assert_eq!(batch.num_columns(), schema.fields().len());
        assert_eq!(batch.schema(), schema);
    }

    #[test]
    fn empty_store_yields_empty_batch() {
        let schema = activity_schema();
        let batch = records_to_batch(&[], &schema).unwrap();
        assert_eq!(batch.num_rows(), 0);
    }
}
