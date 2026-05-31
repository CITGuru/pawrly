//! Per-RPC service implementations. Each one wraps the shared
//! `Arc<dyn EngineService>` and translates between the wire types
//! and core types.

pub(crate) mod admin;
pub(crate) mod cache;
pub(crate) mod catalog;
pub(crate) mod query;
pub(crate) mod semantic;
pub(crate) mod sources;

pub(crate) use admin::AdminSvc;
pub(crate) use cache::CacheSvc;
pub(crate) use catalog::CatalogSvc;
pub(crate) use query::QuerySvc;
pub(crate) use semantic::SemanticSvc;
pub(crate) use sources::SourcesSvc;
