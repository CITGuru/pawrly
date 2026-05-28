//! Encode/decode Arrow `RecordBatch`es to/from the bytes carried in
//! `QueryResponse.ipc_stream`.
//!
//! The encoding is the simplest correct variant: each frame is
//! a complete Arrow IPC stream containing one batch (schema + batch).
//! This costs a few extra bytes per batch but keeps the encoder and decoder
//! stateless; we can switch to schema-once encoding later without changing
//! the wire shape (the reader copes with both).

use std::sync::Arc;

use arrow_array::RecordBatch;
use arrow_ipc::reader::StreamReader;
use arrow_ipc::writer::{IpcWriteOptions, StreamWriter};
use arrow_schema::Schema;
use bytes::Bytes;

/// Encode a single `RecordBatch` into self-contained Arrow IPC stream bytes.
///
/// Returns an error if the IPC writer fails (would only happen on a
/// schema/batch mismatch which is impossible by construction here).
pub fn encode_batch(batch: &RecordBatch) -> Result<Bytes, ArrowIpcError> {
    let schema = batch.schema();
    let mut buf = Vec::with_capacity(estimate_size(batch));
    {
        let options = IpcWriteOptions::default();
        let mut writer = StreamWriter::try_new_with_options(&mut buf, schema.as_ref(), options)
            .map_err(|e| ArrowIpcError::Encode(e.to_string()))?;
        writer
            .write(batch)
            .map_err(|e| ArrowIpcError::Encode(e.to_string()))?;
        writer
            .finish()
            .map_err(|e| ArrowIpcError::Encode(e.to_string()))?;
    }
    Ok(Bytes::from(buf))
}

/// Decode a single Arrow IPC frame back into one or more `RecordBatch`es.
///
/// In normal usage exactly one batch comes back; we collect all in case the
/// peer ever batches multiple records into one frame.
pub fn decode_frame(bytes: &[u8]) -> Result<Vec<RecordBatch>, ArrowIpcError> {
    let reader =
        StreamReader::try_new(bytes, None).map_err(|e| ArrowIpcError::Decode(e.to_string()))?;
    let mut batches = Vec::new();
    for batch in reader {
        batches.push(batch.map_err(|e| ArrowIpcError::Decode(e.to_string()))?);
    }
    Ok(batches)
}

/// Decode just the schema from an IPC frame without consuming the batches.
pub fn decode_schema(bytes: &[u8]) -> Result<Arc<Schema>, ArrowIpcError> {
    let reader =
        StreamReader::try_new(bytes, None).map_err(|e| ArrowIpcError::Decode(e.to_string()))?;
    Ok(reader.schema())
}

fn estimate_size(batch: &RecordBatch) -> usize {
    // Crude but sufficient: total bytes across all columns plus a fixed
    // header allowance.
    let body: usize = batch
        .columns()
        .iter()
        .map(|c| c.get_array_memory_size())
        .sum();
    body + 1024
}

#[derive(Debug, thiserror::Error)]
pub enum ArrowIpcError {
    #[error("arrow ipc encode error: {0}")]
    Encode(String),

    #[error("arrow ipc decode error: {0}")]
    Decode(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::{Int64Array, StringArray};
    use arrow_schema::{DataType, Field};

    fn sample_batch() -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("label", DataType::Utf8, true),
        ]));
        let id = Arc::new(Int64Array::from(vec![1, 2, 3]));
        let label = Arc::new(StringArray::from(vec!["a", "b", "c"]));
        RecordBatch::try_new(schema, vec![id, label]).unwrap()
    }

    #[test]
    fn round_trip() {
        let batch = sample_batch();
        let bytes = encode_batch(&batch).unwrap();
        let decoded = decode_frame(&bytes).unwrap();
        assert_eq!(decoded.len(), 1);
        let got = &decoded[0];
        assert_eq!(got.num_rows(), 3);
        assert_eq!(got.num_columns(), 2);
        assert_eq!(got.schema().field(0).name(), "id");
        assert_eq!(got.schema().field(1).name(), "label");
    }

    #[test]
    fn decode_schema_works() {
        let batch = sample_batch();
        let bytes = encode_batch(&batch).unwrap();
        let schema = decode_schema(&bytes).unwrap();
        assert_eq!(schema.fields().len(), 2);
    }

    #[test]
    fn empty_decoder_input_errs() {
        let r = decode_frame(&[]);
        assert!(r.is_err());
    }
}
