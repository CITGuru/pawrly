//! HTTP/REST source for Pawrly.
//!
//! Typed declarative tables (one GET per table, schema and params
//! declared in YAML), plus the raw HTTP escape-hatch table. Pagination,
//! retries, and rate limiting are stubbed and not yet implemented.

#![doc(html_root_url = "https://docs.rs/pawrly-sources-http")]

mod raw;
mod register;
mod source;
mod typed;

pub use raw::RawHttpTableProvider;
pub use register::{HttpBuildError, HttpSourceReport, HttpTableSummary, register_http_source};
pub use source::{AuthSpec, HttpSource, HttpTableSpec, ParamSpec};
pub use typed::HttpTableProvider;

/// Bundled HTTP source specs (github / linear / stripe).
pub mod bundled;
