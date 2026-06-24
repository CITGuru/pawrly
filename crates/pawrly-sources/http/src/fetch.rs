//! Shared HTTP fetch entrypoint for table-valued functions.
//!
//! Tables and functions run the same fetch pipeline; only the params differ — a
//! table binds them from `WHERE` filters in [`HttpTableProvider::scan`], a
//! function from its call arguments. This delegates to
//! [`HttpTableProvider::scan_params`] so the execution logic has one home
//! (`typed.rs`).

use std::collections::BTreeMap;
use std::sync::Arc;

use arrow_array::RecordBatch;

use crate::source::{HttpSource, HttpTableSpec};
use crate::typed::HttpTableProvider;

/// Run the HTTP fetch pipeline for one fully-bound parameter set. `max_pages`
/// bounds pagination (`None` = the provider's default); the batch schema comes
/// from `spec`, so the function's `returns:` drives the columns.
pub(crate) async fn fetch_batch(
    source: Arc<HttpSource>,
    spec: Arc<HttpTableSpec>,
    params: &BTreeMap<String, String>,
    limit: Option<usize>,
    max_pages: Option<u32>,
) -> datafusion::common::Result<RecordBatch> {
    let provider = HttpTableProvider::with_safety(source, spec, max_pages, None);
    provider.scan_params(params, limit).await
}
