//! CacheService implementation.

use std::sync::Arc;

use pawrly_core::EngineService;
use pawrly_proto::v1::{
    self, InvalidateRequest, InvalidateResponse, ListEntriesRequest, ListEntriesResponse,
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
        _req: Request<ListEntriesRequest>,
    ) -> Result<Response<ListEntriesResponse>, Status> {
        let entries = self
            .engine
            .cache_entries()
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
        Ok(Response::new(ListEntriesResponse { entries: proto }))
    }

    async fn refresh(
        &self,
        req: Request<RefreshRequest>,
    ) -> Result<Response<RefreshResponse>, Status> {
        let name = req
            .into_inner()
            .name
            .ok_or_else(|| Status::invalid_argument("name is required"))?;
        let core_name = pawrly_core::TableName::from(name);
        let r = self
            .engine
            .refresh_table(&core_name)
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
}

fn timestamp(t: chrono::DateTime<chrono::Utc>) -> Timestamp {
    Timestamp {
        seconds: t.timestamp(),
        nanos: t.timestamp_subsec_nanos() as i32,
    }
}
