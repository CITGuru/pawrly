//! HTTP/REST source for Pawrly.
//!
//! Typed declarative tables (one GET per table, schema and params
//! declared in YAML), plus the raw HTTP escape-hatch table. Typed tables
//! support pagination, retries with backoff, and rate limiting.

#![doc(html_root_url = "https://docs.rs/pawrly-sources-http")]

mod paginate;
mod raw;
mod register;
mod source;
mod typed;

pub use raw::RawHttpTableProvider;
pub use register::{HttpBuildError, HttpSourceReport, HttpTableSummary, register_http_source};
pub use source::{
    AuthSpec, HttpSource, HttpTableSpec, PaginationConfig, ParamSpec, RateLimitConfig, RetryConfig,
};
pub use typed::HttpTableProvider;
