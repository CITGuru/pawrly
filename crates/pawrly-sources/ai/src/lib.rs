//! AI/LLM source for Pawrly. Currently provides:
//! - `<source>.chat(model, prompt) -> varchar`
//! - `<source>.models` catalog table
//!
//! Other UDFs (classify / summarize / extract / embed) and batching are not yet implemented.

#![doc(html_root_url = "https://docs.rs/pawrly-sources-ai")]

mod models_table;
mod register;
mod udf;

pub use register::{AiBuildError, AiSourceReport, register_ai_source};
