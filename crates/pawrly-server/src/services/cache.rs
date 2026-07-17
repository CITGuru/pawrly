//! CacheService implementation.

use std::sync::Arc;

use pawrly_core::EngineService;
use pawrly_proto::v1::{
    self, DropMaterializedRequest, DropMaterializedResponse, InvalidateRequest, InvalidateResponse,
    ListEntriesRequest, ListEntriesResponse, MaterializeRequest, MaterializeResponse,
    RefreshRequest, RefreshResponse, VacuumRequest, VacuumResponse,
    cache_service_server::CacheService,
};
use prost_types::Timestamp;
use tonic::{Request, Response, Status, async_trait};

use crate::error::engine_error_to_status;

pub(crate) struct CacheSvc {
    engine: Arc<dyn EngineService>,
}

impl CacheSvc {
    pub(crate) fn new(engine: Arc<dyn EngineService>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl CacheService for CacheSvc {
    async fn list_entries(
        &self,
        req: Request<ListEntriesRequest>,
    ) -> Result<Response<ListEntriesResponse>, Status> {
        let namespace = req.into_inner().namespace;
        let entries = self
            .engine
            .cache_entries(none_if_empty(&namespace))
            .await
            .map_err(|e| engine_error_to_status(&e))?;
        let proto = entries
            .into_iter()
            .map(|e| v1::CacheEntryInfo {
                name: Some((&e.name).into()),
                mode: v1::CacheMode::from(e.mode) as i32,
                written_at: Some(timestamp(e.written_at)),
                expires_at: e.expires_at.map(timestamp),
                row_count: e.row_count,
                size_bytes: e.size_bytes,
                file_count: e.file_count,
            })
            .collect();
        Ok(Response::new(ListEntriesResponse {
            entries: proto,
            namespace,
        }))
    }

    async fn refresh(
        &self,
        req: Request<RefreshRequest>,
    ) -> Result<Response<RefreshResponse>, Status> {
        let req = req.into_inner();
        let name = req
            .name
            .ok_or_else(|| Status::invalid_argument("name is required"))?;
        let core_name = pawrly_core::TableName::from(name);
        let r = self
            .engine
            .refresh_table(&core_name, none_if_empty(&req.namespace))
            .await
            .map_err(|e| engine_error_to_status(&e))?;
        Ok(Response::new(RefreshResponse {
            rows_written: r.rows_written,
            size_bytes: r.size_bytes,
            elapsed: Some(prost_types::Duration {
                seconds: r.elapsed.as_secs() as i64,
                nanos: r.elapsed.subsec_nanos() as i32,
            }),
            expires_at: r.expires_at.map(timestamp),
            namespace: req.namespace,
        }))
    }

    async fn invalidate(
        &self,
        req: Request<InvalidateRequest>,
    ) -> Result<Response<InvalidateResponse>, Status> {
        let name = req
            .into_inner()
            .name
            .ok_or_else(|| Status::invalid_argument("name is required"))?;
        let core_name = pawrly_core::TableName::from(name);
        let removed = self
            .engine
            .invalidate_cache(&core_name)
            .await
            .map_err(|e| engine_error_to_status(&e))?;
        Ok(Response::new(InvalidateResponse { removed }))
    }

    async fn vacuum(
        &self,
        _req: Request<VacuumRequest>,
    ) -> Result<Response<VacuumResponse>, Status> {
        let r = self
            .engine
            .vacuum_cache()
            .await
            .map_err(|e| engine_error_to_status(&e))?;
        Ok(Response::new(VacuumResponse {
            entries_removed: r.entries_removed,
            files_removed: r.files_removed,
            bytes_reclaimed: r.bytes_reclaimed,
        }))
    }

    async fn materialize(
        &self,
        req: Request<MaterializeRequest>,
    ) -> Result<Response<MaterializeResponse>, Status> {
        let req = req.into_inner();
        let spec = req
            .spec
            .ok_or_else(|| Status::invalid_argument("spec is required"))?;
        let core_spec = pawrly_core::MaterializeSpec::try_from(spec)?;
        let outcome = self
            .engine
            .materialize(&req.name, core_spec, none_if_empty(&req.namespace))
            .await
            .map_err(|e| engine_error_to_status(&e))?;
        let mut resp: MaterializeResponse = outcome.into();
        resp.namespace = req.namespace;
        Ok(Response::new(resp))
    }

    async fn drop_materialized(
        &self,
        req: Request<DropMaterializedRequest>,
    ) -> Result<Response<DropMaterializedResponse>, Status> {
        let req = req.into_inner();
        let dropped = self
            .engine
            .drop_materialized(&req.name, none_if_empty(&req.namespace))
            .await
            .map_err(|e| engine_error_to_status(&e))?;
        Ok(Response::new(DropMaterializedResponse {
            dropped,
            namespace: req.namespace,
        }))
    }
}

fn none_if_empty(s: &str) -> Option<&str> {
    if s.is_empty() { None } else { Some(s) }
}

fn timestamp(t: chrono::DateTime<chrono::Utc>) -> Timestamp {
    Timestamp {
        seconds: t.timestamp(),
        nanos: t.timestamp_subsec_nanos() as i32,
    }
}
