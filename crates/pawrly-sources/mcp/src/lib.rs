//! MCP source for Pawrly.
//!
//! Connects to an external MCP server (stdio subprocess or streamable HTTP),
//! discovers its tools via `tools/list`, and exposes them as SQL tables backed
//! by `tools/call`. Tables come from introspection (gated by `expose`) and/or
//! declarative `tables:` entries that patch a synthesized table or define a new
//! tool-backed one.

mod error;
mod http;
mod provider;
mod register;
mod session;
mod stdio;
mod synth;
mod transport;

pub use error::McpBuildError;
pub use register::{McpSourceReport, McpTableSummary, register_mcp_source};
