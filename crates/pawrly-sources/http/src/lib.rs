//! HTTP/REST source for Pawrly.
//!
//! Typed declarative tables (one GET per table, schema and params
//! declared in YAML), plus the raw HTTP escape-hatch table. Typed tables
//! support pagination, retries with backoff, and rate limiting.

#![doc(html_root_url = "https://docs.rs/pawrly-sources-http")]

mod deferred;
mod dependent_join;
mod expr;
mod openapi;
mod paginate;
mod raw;
mod register;
mod source;
mod typed;

pub use deferred::DeferredHttpScanExec;
pub use dependent_join::{DependentJoinExec, DependentJoinRule};
pub use raw::RawHttpTableProvider;
pub use register::{HttpBuildError, HttpSourceReport, HttpTableSummary, register_http_source};
pub use source::{
    AuthHeader, AuthSpec, HttpSource, HttpTableSpec, PaginationConfig, ParamSpec, QueryCredential,
    RateLimitConfig, RetryConfig,
};
pub use typed::HttpTableProvider;
