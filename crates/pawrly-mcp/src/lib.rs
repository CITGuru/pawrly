//! MCP (Model Context Protocol) server for Pawrly.
//!
//! A minimal stdio JSON-RPC server. Each tool proxies one or two
//! [`EngineService`](pawrly_core::EngineService) calls:
//!
//! - catalog — `list_sources`, `list_tables`, `describe_table`, `get_schema`
//! - query — `query` (raw SQL, returns `{ columns, rows, row_count }`),
//!   `cancel_query`
//! - cache — `refresh_table`
//! - materialized tables — `materialize`, `drop_materialized`
//! - semantic layer — `list_semantic_models`, `describe_semantic_model`,
//!   `semantic_query`
//!
//! Tools are exposed over a stdio transport ([`serve_stdio`]) and an HTTP
//! transport ([`serve_http`]), both driven by a shared JSON-RPC dispatcher.
//!
//! MCP resources/prompts, OAuth2, audit logs, and Prometheus metrics are not
//! yet implemented.

#![doc(html_root_url = "https://docs.rs/pawrly-mcp")]

mod cancel;
mod dispatch;
mod http;
mod stdio;
mod tools;

pub use cancel::CancelRegistry;
pub use dispatch::handle_message;
pub use http::{HttpOpts, serve_http};
pub use stdio::serve_stdio;
pub use tools::{call_tool, list_tools};
