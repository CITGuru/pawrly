//! SourcesService implementation.

use std::sync::Arc;

use pawrly_core::{EngineService, SourceDef};
use pawrly_proto::v1::{
    self, AddSourceRequest, AddSourceResponse, ListSourcesRequest, ListSourcesResponse,
    ReloadConfigRequest, ReloadConfigResponse, RemoveSourceRequest, RemoveSourceResponse,
    TestSourceRequest, TestSourceResponse, sources_service_server::SourcesService,
};
use tonic::{Request, Response, Status, async_trait};

use crate::error::engine_error_to_status;

pub(crate) struct SourcesSvc {
    engine: Arc<dyn EngineService>,
}

impl SourcesSvc {
    pub(crate) fn new(engine: Arc<dyn EngineService>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl SourcesService for SourcesSvc {
    async fn list_sources(
        &self,
        _req: Request<ListSourcesRequest>,
    ) -> Result<Response<ListSourcesResponse>, Status> {
        let sources = self
            .engine
            .list_sources()
            .await
            .map_err(|e| engine_error_to_status(&e))?;
        Ok(Response::new(ListSourcesResponse {
            sources: sources.into_iter().map(v1::SourceInfo::from).collect(),
        }))
    }

    async fn add_source(
        &self,
        req: Request<AddSourceRequest>,
    ) -> Result<Response<AddSourceResponse>, Status> {
        let def: SourceDef = serde_yaml::from_str(&req.into_inner().yaml)
            .map_err(|e| Status::invalid_argument(format!("invalid source YAML: {e}")))?;
        let info = self
            .engine
            .add_source(def)
            .await
            .map_err(|e| engine_error_to_status(&e))?;
        Ok(Response::new(AddSourceResponse {
            source: Some(v1::SourceInfo::from(info)),
        }))
    }

    async fn remove_source(
        &self,
        req: Request<RemoveSourceRequest>,
    ) -> Result<Response<RemoveSourceResponse>, Status> {
        let removed = self
            .engine
            .remove_source(&req.into_inner().name)
            .await
            .map_err(|e| engine_error_to_status(&e))?;
        Ok(Response::new(RemoveSourceResponse { removed }))
    }

    async fn test_source(
        &self,
        req: Request<TestSourceRequest>,
    ) -> Result<Response<TestSourceResponse>, Status> {
        let r = self
            .engine
            .test_source(&req.into_inner().name)
            .await
            .map_err(|e| engine_error_to_status(&e))?;
        Ok(Response::new(TestSourceResponse {
            ok: r.ok,
            latency: Some(prost_types::Duration {
                seconds: r.latency.as_secs() as i64,
                nanos: r.latency.subsec_nanos() as i32,
            }),
            detail: r.detail.unwrap_or_default(),
        }))
    }

    async fn reload_config(
        &self,
        _req: Request<ReloadConfigRequest>,
    ) -> Result<Response<ReloadConfigResponse>, Status> {
        let r = self
            .engine
            .reload_config()
            .await
            .map_err(|e| engine_error_to_status(&e))?;
        Ok(Response::new(ReloadConfigResponse {
            sources_added: r.sources_added,
            sources_removed: r.sources_removed,
            sources_changed: r.sources_changed,
        }))
    }
}
