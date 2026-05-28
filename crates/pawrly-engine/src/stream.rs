//! Adapt DataFusion's `SendableRecordBatchStream` (which yields
//! `Result<RecordBatch, DataFusionError>`) into the trait's `QueryStream`
//! (which yields `Result<RecordBatch, EngineError>`).

use datafusion::physical_plan::SendableRecordBatchStream;
use futures_util::StreamExt as _;
use pawrly_core::{EngineError, QueryStream};

pub fn adapt(inner: SendableRecordBatchStream) -> QueryStream {
    let mapped =
        inner.map(|item| item.map_err(|e| EngineError::Internal(format!("datafusion: {e}"))));
    Box::pin(mapped) as QueryStream
}
