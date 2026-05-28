//! AdminService: health, version, shutdown.

use std::sync::Arc;

use pawrly_core::EngineService;
use pawrly_proto::v1::{
    self, HealthRequest, HealthResponse, ShutdownRequest, ShutdownResponse, VersionRequest,
    VersionResponse, admin_service_server::AdminService,
};
use tonic::{Request, Response, Status, async_trait};

use crate::error::engine_error_to_status;

pub(crate) struct AdminSvc {
    engine: Arc<dyn EngineService>,
}

impl AdminSvc {
    pub(crate) fn new(engine: Arc<dyn EngineService>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl AdminService for AdminSvc {
    async fn health(
        &self,
        _req: Request<HealthRequest>,
    ) -> Result<Response<HealthResponse>, Status> {
        let r = self
            .engine
            .health()
            .await
            .map_err(|e| engine_error_to_status(&e))?;
        Ok(Response::new(v1::HealthResponse::from(r)))
    }

    async fn shutdown(
        &self,
        _req: Request<ShutdownRequest>,
    ) -> Result<Response<ShutdownResponse>, Status> {
        // Don't actually shut the process down for arbitrary callers.
        // The CLI implements `pawrly stop` via SIGTERM + pid file.
        Ok(Response::new(ShutdownResponse { accepted: false }))
    }

    async fn version(
        &self,
        _req: Request<VersionRequest>,
    ) -> Result<Response<VersionResponse>, Status> {
        Ok(Response::new(VersionResponse {
            pawrly_version: env!("CARGO_PKG_VERSION").into(),
            api_version: "v1".into(),
            protobuf_revision: option_env!("PAWRLY_PROTO_REV").unwrap_or("dev").into(),
        }))
    }
}
