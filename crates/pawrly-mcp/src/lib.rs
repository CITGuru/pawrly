//! MCP (Model Context Protocol) server for Pawrly.
//!
//! A minimal stdio JSON-RPC server with two tools:
//! - `list_tables` — proxies `EngineService::list_tables`
//! - `query` — proxies `EngineService::query`, returns rows as a compact
//!   JSON object `{ columns, rows, row_count }`.
//!
//! HTTP+SSE transport, OAuth2, audit logs, and the full six-tool surface
//! are not yet implemented.

#![doc(html_root_url = "https://docs.rs/pawrly-mcp")]

mod stdio;
mod tools;

pub use stdio::serve_stdio;
pub use tools::{call_tool, list_tools};
