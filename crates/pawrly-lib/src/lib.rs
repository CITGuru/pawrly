//! Public Rust library facade for Pawrly.
//!
//! This is the only crate in the workspace with semver guarantees in v1.
//! Downstream Rust users should depend on this crate (named `pawrly` on crates.io)
//! rather than reaching directly into `pawrly-core`, `pawrly-engine`, etc.

pub use pawrly_core as core;
