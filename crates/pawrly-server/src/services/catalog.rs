//! CatalogService implementation.

use std::sync::Arc;

use pawrly_core::{EngineService, TableFilter};
use pawrly_proto::v1::{
    self, DescribeFunctionRequest, DescribeFunctionResponse, DescribeTableRequest,
    DescribeTableResponse, ListFunctionsRequest, ListFunctionsResponse, ListTablesRequest,
    ListTablesResponse, RefreshCatalogRequest, RefreshCatalogResponse, SchemaSnapshotRequest,
    SchemaSnapshotResponse, catalog_service_server::CatalogService,
};
use tonic::{Request, Response, Status, async_trait};

use crate::error::engine_error_to_status;

pub(crate) struct CatalogSvc {
    engine: Arc<dyn EngineService>,
}

impl CatalogSvc {
    pub(crate) fn new(engine: Arc<dyn EngineService>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl CatalogService for CatalogSvc {
    async fn list_tables(
        &self,
        req: Request<ListTablesRequest>,
    ) -> Result<Response<ListTablesResponse>, Status> {
        let req = req.into_inner();
        let filter = if req.source.is_some() || req.name_glob.is_some() {
            Some(TableFilter {
                source: req.source,
                name_glob: req.name_glob,
            })
        } else {
            None
        };
        let tables = self
            .engine
            .list_tables(filter)
            .await
            .map_err(|e| engine_error_to_status(&e))?;
        Ok(Response::new(ListTablesResponse {
            tables: tables.into_iter().map(v1::TableInfo::from).collect(),
        }))
    }

    async fn describe_table(
        &self,
        req: Request<DescribeTableRequest>,
    ) -> Result<Response<DescribeTableResponse>, Status> {
        let req = req.into_inner();
        let name = req
            .name
            .ok_or_else(|| Status::invalid_argument("name is required"))?;
        let core_name = pawrly_core::TableName::from(name);
        let desc = self
            .engine
            .describe_table(&core_name)
            .await
            .map_err(|e| engine_error_to_status(&e))?;
        Ok(Response::new(DescribeTableResponse {
            table: Some(desc.table.into()),
            columns: desc.columns.into_iter().map(v1::ColumnSpec::from).collect(),
            pushable_filter_columns: desc.pushable_filter_columns,
            examples: desc.examples,
            wiki: desc.wiki,
        }))
    }

    async fn schema_snapshot(
        &self,
        req: Request<SchemaSnapshotRequest>,
    ) -> Result<Response<SchemaSnapshotResponse>, Status> {
        let req = req.into_inner();
        let snap = self
            .engine
            .schema_snapshot(
                if req.sources.is_empty() {
                    None
                } else {
                    Some(req.sources)
                },
                req.compact,
            )
            .await
            .map_err(|e| engine_error_to_status(&e))?;
        let json = serde_json::to_vec(&snap)
            .map_err(|e| Status::internal(format!("schema snapshot serialize: {e}")))?;
        Ok(Response::new(SchemaSnapshotResponse {
            snapshot_json: json,
        }))
    }

    async fn refresh_catalog(
        &self,
        req: Request<RefreshCatalogRequest>,
    ) -> Result<Response<RefreshCatalogResponse>, Status> {
        let r = self
            .engine
            .refresh_catalog(req.into_inner().source.as_deref())
            .await
            .map_err(|e| engine_error_to_status(&e))?;
        Ok(Response::new(RefreshCatalogResponse {
            sources_refreshed: r.sources_refreshed,
            tables_discovered: r.tables_discovered,
        }))
    }

    async fn list_functions(
        &self,
        _req: Request<ListFunctionsRequest>,
    ) -> Result<Response<ListFunctionsResponse>, Status> {
        let functions = self
            .engine
            .list_functions()
            .await
            .map_err(|e| engine_error_to_status(&e))?;
        Ok(Response::new(ListFunctionsResponse {
            functions: functions.into_iter().map(v1::FunctionInfo::from).collect(),
        }))
    }

    async fn describe_function(
        &self,
        req: Request<DescribeFunctionRequest>,
    ) -> Result<Response<DescribeFunctionResponse>, Status> {
        let req = req.into_inner();
        let d = self
            .engine
            .describe_function(&req.namespace, &req.name)
            .await
            .map_err(|e| engine_error_to_status(&e))?;
        Ok(Response::new(DescribeFunctionResponse {
            function: Some(v1::FunctionDescription::from(d)),
        }))
    }
}
