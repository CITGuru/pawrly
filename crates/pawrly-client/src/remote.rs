//! `RemoteEngineClient` — implements `EngineService` over a tonic channel.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use futures_util::StreamExt as _;
use pawrly_core::semantic::{SemanticModelDescription, SemanticModelInfo, SemanticQuery};
use pawrly_core::{
    CacheEntryInfo, CacheMode, CatalogSnapshot, EngineError, EngineService, HealthReport,
    MaterializeOutcome, MaterializeSpec, QueryCompleted, QueryCompletion, QueryHandle, QueryId,
    QueryRequest, RefreshCatalogOutcome, RefreshOutcome, ReloadReport, SourceDef, SourceInfo,
    SourceTestReport, TableDescription, TableFilter, TableInfo, TableName, VacuumReport,
};
use pawrly_proto::arrow_helpers::decode_frame;
use pawrly_proto::conv::{engine_error_to_status, status_to_engine_error};
use pawrly_proto::v1::{
    self, AddSourceRequest, CancelRequest, DescribeModelRequest, DropMaterializedRequest,
    ExplainRequest, HealthRequest, InvalidateRequest, ListEntriesRequest, ListModelsRequest,
    ListSourcesRequest, ListTablesRequest, MaterializeRequest, RefreshCatalogRequest,
    RefreshRequest, ReloadConfigRequest, RemoveSourceRequest, SchemaSnapshotRequest,
    TestSourceRequest, VacuumRequest, admin_service_client::AdminServiceClient,
    cache_service_client::CacheServiceClient, catalog_service_client::CatalogServiceClient,
    query_response::Payload as QueryPayload, query_service_client::QueryServiceClient,
    semantic_service_client::SemanticServiceClient, sources_service_client::SourcesServiceClient,
};
use tonic::service::interceptor::InterceptedService;
use tonic::transport::Channel;
use tonic::{Request, Status};
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::transport::{ConnectError, Endpoint};

/// Injects `authorization: Bearer <token>` into every outgoing request when a
/// token is configured; a no-op otherwise.
#[derive(Clone, Default)]
pub struct BearerInjector {
    token: Option<Arc<str>>,
}

impl tonic::service::Interceptor for BearerInjector {
    fn call(&mut self, mut req: Request<()>) -> Result<Request<()>, Status> {
        if let Some(token) = &self.token {
            let value = format!("Bearer {token}")
                .parse()
                .map_err(|_| Status::invalid_argument("invalid bearer token"))?;
            req.metadata_mut().insert("authorization", value);
        }
        // Carry the active trace context so the daemon-side span joins this
        // trace. No-op when OTel is disabled (the global propagator is a no-op).
        let cx = tracing::Span::current().context();
        pawrly_proto::propagation::inject_context(&cx, req.metadata_mut());
        Ok(req)
    }
}

/// A channel with the bearer-injecting interceptor applied.
type AuthChannel = InterceptedService<Channel, BearerInjector>;

/// Pawrly engine accessed over gRPC.
#[derive(Clone)]
pub struct RemoteEngineClient {
    query: QueryServiceClient<AuthChannel>,
    catalog: CatalogServiceClient<AuthChannel>,
    sources: SourcesServiceClient<AuthChannel>,
    cache: CacheServiceClient<AuthChannel>,
    semantic: SemanticServiceClient<AuthChannel>,
    admin: AdminServiceClient<AuthChannel>,
}

impl RemoteEngineClient {
    /// Open all service clients on the given endpoint, injecting its bearer
    /// token (if any) into every request.
    pub async fn connect(endpoint: Endpoint) -> Result<Self, ConnectError> {
        let injector = BearerInjector {
            token: endpoint.bearer_token().map(|t| Arc::from(t.as_str())),
        };
        let channel = endpoint.connect().await?;
        Ok(Self {
            query: QueryServiceClient::with_interceptor(channel.clone(), injector.clone()),
            catalog: CatalogServiceClient::with_interceptor(channel.clone(), injector.clone()),
            sources: SourcesServiceClient::with_interceptor(channel.clone(), injector.clone()),
            cache: CacheServiceClient::with_interceptor(channel.clone(), injector.clone()),
            semantic: SemanticServiceClient::with_interceptor(channel.clone(), injector.clone()),
            admin: AdminServiceClient::with_interceptor(channel, injector),
        })
    }
}

fn dur(d: Option<prost_types::Duration>) -> Duration {
    d.and_then(|d| {
        let secs = u64::try_from(d.seconds).ok()?;
        let nanos = u32::try_from(d.nanos).ok()?;
        Some(Duration::new(secs, nanos))
    })
    .unwrap_or(Duration::ZERO)
}

fn require_namespace_echo(requested: Option<&str>, echoed: &str) -> Result<(), EngineError> {
    match requested {
        Some(ns) if !ns.is_empty() && ns != echoed => Err(EngineError::Protocol(format!(
            "daemon ignored namespace `{ns}` — it predates materialize namespaces, so the \
             operation targeted the default namespace instead; upgrade the daemon"
        ))),
        _ => Ok(()),
    }
}

fn ts(t: prost_types::Timestamp) -> chrono::DateTime<chrono::Utc> {
    let nanos = u32::try_from(t.nanos).unwrap_or(0);
    chrono::TimeZone::timestamp_opt(&Utc, t.seconds, nanos)
        .single()
        .unwrap_or_else(Utc::now)
}

/// Pull the leading `Started` frame and return its `query_id` (so `cancel` has
/// it before the caller drains). A leading `Error` frame is propagated; any
/// other/absent leading frame is a protocol error.
async fn read_started_id(
    stream: &mut tonic::Streaming<v1::QueryResponse>,
) -> Result<QueryId, EngineError> {
    match stream.next().await {
        Some(frame) => match frame.map_err(status_to_engine_error)?.payload {
            Some(QueryPayload::Started(s)) => Ok(QueryId::new(s.query_id)),
            Some(QueryPayload::Error(err)) => {
                let mut st = Status::internal(err.message.clone());
                if let Ok(v) = err.code.parse() {
                    st.metadata_mut().insert("pawrly-error-code", v);
                }
                Err(status_to_engine_error(st))
            }
            _ => Err(EngineError::Protocol(
                "expected a Started frame first".into(),
            )),
        },
        None => Ok(QueryId::new(String::new())),
    }
}

/// Convert a proto `QueryCompleted` frame into the core completion metadata.
fn proto_completed(c: &v1::QueryCompleted) -> QueryCompleted {
    let elapsed = c
        .elapsed
        .as_ref()
        .map(|d| Duration::new(d.seconds.max(0) as u64, d.nanos.max(0) as u32))
        .unwrap_or_default();
    QueryCompleted {
        rows_returned: c.rows_returned,
        truncated: c.truncated,
        elapsed,
    }
}

#[async_trait]
impl EngineService for RemoteEngineClient {
    async fn query(&self, req: QueryRequest) -> Result<QueryHandle, EngineError> {
        let mut client = self.query.clone();
        let proto: v1::QueryRequest = req.into();
        let mut server_stream = client
            .query(proto)
            .await
            .map_err(status_to_engine_error)?
            .into_inner();

        // Capture the id up front (for `cancel`); `Completed` fills `completion`.
        let query_id = read_started_id(&mut server_stream).await?;
        let completion: QueryCompletion = Arc::new(std::sync::OnceLock::new());
        let completion_slot = completion.clone();

        let stream = async_stream::try_stream! {
            while let Some(frame) = server_stream.next().await {
                let frame = frame.map_err(status_to_engine_error)?;
                let Some(payload) = frame.payload else { continue; };
                match payload {
                    QueryPayload::Started(_) => continue,
                    QueryPayload::IpcStream(bytes) => {
                        let batches = decode_frame(&bytes)
                            .map_err(|e| EngineError::Protocol(format!("ipc decode: {e}")))?;
                        for b in batches {
                            yield b;
                        }
                    }
                    QueryPayload::Completed(c) => {
                        let _ = completion_slot.set(proto_completed(&c));
                    }
                    QueryPayload::Error(err) => {
                        let mut s = tonic::Status::internal(err.message.clone());
                        if let Ok(v) = err.code.parse() {
                            s.metadata_mut().insert("pawrly-error-code", v);
                        }
                        Err(status_to_engine_error(s))?;
                    }
                }
            }
        };

        Ok(QueryHandle::new(query_id, Box::pin(stream), completion))
    }

    async fn explain(&self, sql: &str, analyze: bool) -> Result<String, EngineError> {
        let mut client = self.query.clone();
        let resp = client
            .explain(ExplainRequest {
                sql: sql.to_string(),
                analyze,
                params: Default::default(),
            })
            .await
            .map_err(status_to_engine_error)?
            .into_inner();
        Ok(resp.plan)
    }

    async fn cancel(&self, query_id: &QueryId) -> Result<bool, EngineError> {
        let mut client = self.query.clone();
        let resp = client
            .cancel(CancelRequest {
                query_id: query_id.0.clone(),
            })
            .await
            .map_err(status_to_engine_error)?
            .into_inner();
        Ok(resp.cancelled)
    }

    async fn list_sources(&self) -> Result<Vec<SourceInfo>, EngineError> {
        let mut client = self.sources.clone();
        let resp = client
            .list_sources(ListSourcesRequest {})
            .await
            .map_err(status_to_engine_error)?
            .into_inner();
        let mut out = Vec::with_capacity(resp.sources.len());
        for s in resp.sources {
            out.push(s.try_into().map_err(|e: pawrly_proto::conv::ConvError| {
                EngineError::Protocol(e.to_string())
            })?);
        }
        Ok(out)
    }

    async fn list_tables(
        &self,
        filter: Option<TableFilter>,
    ) -> Result<Vec<TableInfo>, EngineError> {
        let mut client = self.catalog.clone();
        let req = filter.map_or(
            ListTablesRequest {
                source: None,
                name_glob: None,
            },
            |f| ListTablesRequest {
                source: f.source,
                name_glob: f.name_glob,
            },
        );
        let resp = client
            .list_tables(req)
            .await
            .map_err(status_to_engine_error)?
            .into_inner();
        let mut out = Vec::with_capacity(resp.tables.len());
        for t in resp.tables {
            out.push(t.try_into().map_err(|e: pawrly_proto::conv::ConvError| {
                EngineError::Protocol(e.to_string())
            })?);
        }
        Ok(out)
    }

    async fn describe_table(&self, name: &TableName) -> Result<TableDescription, EngineError> {
        let mut client = self.catalog.clone();
        let resp = client
            .describe_table(pawrly_proto::v1::DescribeTableRequest {
                name: Some(name.into()),
            })
            .await
            .map_err(status_to_engine_error)?
            .into_inner();
        let table_proto = resp
            .table
            .ok_or_else(|| EngineError::Protocol("describe_table response missing table".into()))?;
        let table = table_proto
            .try_into()
            .map_err(|e: pawrly_proto::conv::ConvError| EngineError::Protocol(e.to_string()))?;
        Ok(TableDescription {
            table,
            columns: resp.columns.into_iter().map(Into::into).collect(),
            pushable_filter_columns: resp.pushable_filter_columns,
            examples: resp.examples,
            wiki: resp.wiki,
        })
    }

    async fn list_functions(&self) -> Result<Vec<pawrly_core::FunctionInfo>, EngineError> {
        let mut client = self.catalog.clone();
        let resp = client
            .list_functions(pawrly_proto::v1::ListFunctionsRequest {})
            .await
            .map_err(status_to_engine_error)?
            .into_inner();
        Ok(resp.functions.into_iter().map(Into::into).collect())
    }

    async fn describe_function(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<pawrly_core::FunctionDescription, EngineError> {
        let mut client = self.catalog.clone();
        let resp = client
            .describe_function(pawrly_proto::v1::DescribeFunctionRequest {
                namespace: namespace.to_string(),
                name: name.to_string(),
            })
            .await
            .map_err(status_to_engine_error)?
            .into_inner();
        resp.function.map(Into::into).ok_or_else(|| {
            EngineError::Protocol("describe_function response missing function".into())
        })
    }

    async fn schema_snapshot(
        &self,
        sources: Option<Vec<String>>,
        compact: bool,
    ) -> Result<CatalogSnapshot, EngineError> {
        let mut client = self.catalog.clone();
        let resp = client
            .schema_snapshot(SchemaSnapshotRequest {
                sources: sources.unwrap_or_default(),
                compact,
            })
            .await
            .map_err(status_to_engine_error)?
            .into_inner();
        serde_json::from_slice(&resp.snapshot_json)
            .map_err(|e| EngineError::Protocol(format!("schema snapshot decode: {e}")))
    }

    async fn refresh_catalog(
        &self,
        source: Option<&str>,
    ) -> Result<RefreshCatalogOutcome, EngineError> {
        let mut client = self.catalog.clone();
        let resp = client
            .refresh_catalog(RefreshCatalogRequest {
                source: source.map(str::to_string),
            })
            .await
            .map_err(status_to_engine_error)?
            .into_inner();
        Ok(RefreshCatalogOutcome {
            sources_refreshed: resp.sources_refreshed,
            tables_discovered: resp.tables_discovered,
        })
    }

    async fn cache_entries(
        &self,
        namespace: Option<&str>,
    ) -> Result<Vec<CacheEntryInfo>, EngineError> {
        let mut client = self.cache.clone();
        let resp = client
            .list_entries(ListEntriesRequest {
                namespace: namespace.unwrap_or_default().to_string(),
            })
            .await
            .map_err(status_to_engine_error)?
            .into_inner();
        require_namespace_echo(namespace, &resp.namespace)?;
        let entries = resp
            .entries
            .into_iter()
            .filter_map(|e| {
                let name = e.name?.into();
                let mode = match v1::CacheMode::try_from(e.mode) {
                    Ok(v1::CacheMode::None) => CacheMode::None,
                    Ok(v1::CacheMode::Ttl) => CacheMode::Ttl,
                    Ok(v1::CacheMode::Refresh) => CacheMode::Refresh,
                    Ok(v1::CacheMode::Cron) => CacheMode::Cron,
                    Ok(v1::CacheMode::Append) => CacheMode::Append,
                    Ok(v1::CacheMode::Pinned) => CacheMode::Pinned,
                    // An unknown (newer) mode must not drop the row.
                    Ok(v1::CacheMode::Unspecified) | Err(_) => {
                        if e.expires_at.is_none() {
                            CacheMode::Pinned
                        } else {
                            CacheMode::Ttl
                        }
                    }
                };
                Some(CacheEntryInfo {
                    name,
                    mode,
                    written_at: e.written_at.map(ts).unwrap_or_else(Utc::now),
                    expires_at: e.expires_at.map(ts),
                    row_count: e.row_count,
                    size_bytes: e.size_bytes,
                    file_count: e.file_count,
                })
            })
            .collect();
        Ok(entries)
    }

    async fn refresh_table(&self, name: &TableName) -> Result<RefreshOutcome, EngineError> {
        let mut client = self.cache.clone();
        let resp = client
            .refresh(RefreshRequest {
                name: Some(name.into()),
            })
            .await
            .map_err(status_to_engine_error)?
            .into_inner();
        Ok(RefreshOutcome {
            table: name.clone(),
            rows_written: resp.rows_written,
            size_bytes: resp.size_bytes,
            elapsed: dur(resp.elapsed),
            expires_at: resp.expires_at.map(ts),
        })
    }

    async fn invalidate_cache(&self, name: &TableName) -> Result<bool, EngineError> {
        let mut client = self.cache.clone();
        let resp = client
            .invalidate(InvalidateRequest {
                name: Some(name.into()),
            })
            .await
            .map_err(status_to_engine_error)?
            .into_inner();
        Ok(resp.removed)
    }

    async fn vacuum_cache(&self) -> Result<VacuumReport, EngineError> {
        let mut client = self.cache.clone();
        let resp = client
            .vacuum(VacuumRequest {})
            .await
            .map_err(status_to_engine_error)?
            .into_inner();
        Ok(VacuumReport {
            entries_removed: resp.entries_removed,
            files_removed: resp.files_removed,
            bytes_reclaimed: resp.bytes_reclaimed,
        })
    }

    async fn materialize(
        &self,
        name: &str,
        spec: MaterializeSpec,
        namespace: Option<&str>,
    ) -> Result<MaterializeOutcome, EngineError> {
        let mut client = self.cache.clone();
        let resp = client
            .materialize(MaterializeRequest {
                name: name.to_string(),
                spec: Some(spec.into()),
                namespace: namespace.unwrap_or_default().to_string(),
            })
            .await
            .map_err(status_to_engine_error)?
            .into_inner();
        require_namespace_echo(namespace, &resp.namespace)?;
        Ok(resp.into())
    }

    async fn drop_materialized(
        &self,
        name: &str,
        namespace: Option<&str>,
    ) -> Result<bool, EngineError> {
        let mut client = self.cache.clone();
        let resp = client
            .drop_materialized(DropMaterializedRequest {
                name: name.to_string(),
                namespace: namespace.unwrap_or_default().to_string(),
            })
            .await
            .map_err(status_to_engine_error)?
            .into_inner();
        require_namespace_echo(namespace, &resp.namespace)?;
        Ok(resp.dropped)
    }

    async fn add_source(&self, def: SourceDef) -> Result<SourceInfo, EngineError> {
        let yaml = serde_yaml::to_string(&def)
            .map_err(|e| EngineError::Protocol(format!("serialize source: {e}")))?;
        let mut client = self.sources.clone();
        let resp = client
            .add_source(AddSourceRequest { yaml })
            .await
            .map_err(status_to_engine_error)?
            .into_inner();
        resp.source
            .ok_or_else(|| EngineError::Protocol("AddSourceResponse missing source".into()))?
            .try_into()
            .map_err(|e: pawrly_proto::conv::ConvError| EngineError::Protocol(e.to_string()))
    }

    async fn remove_source(&self, name: &str) -> Result<bool, EngineError> {
        let mut client = self.sources.clone();
        let resp = client
            .remove_source(RemoveSourceRequest {
                name: name.to_string(),
            })
            .await
            .map_err(status_to_engine_error)?
            .into_inner();
        Ok(resp.removed)
    }

    async fn test_source(&self, name: &str) -> Result<SourceTestReport, EngineError> {
        let mut client = self.sources.clone();
        let resp = client
            .test_source(TestSourceRequest {
                name: name.to_string(),
            })
            .await
            .map_err(status_to_engine_error)?
            .into_inner();
        Ok(SourceTestReport {
            name: name.to_string(),
            ok: resp.ok,
            latency: dur(resp.latency),
            detail: if resp.detail.is_empty() {
                None
            } else {
                Some(resp.detail)
            },
        })
    }

    async fn reload_config(&self) -> Result<ReloadReport, EngineError> {
        let mut client = self.sources.clone();
        let resp = client
            .reload_config(ReloadConfigRequest {})
            .await
            .map_err(status_to_engine_error)?
            .into_inner();
        Ok(ReloadReport {
            sources_added: resp.sources_added,
            sources_removed: resp.sources_removed,
            sources_changed: resp.sources_changed,
        })
    }

    async fn list_semantic_models(&self) -> Result<Vec<SemanticModelInfo>, EngineError> {
        let mut client = self.semantic.clone();
        let resp = client
            .list_models(ListModelsRequest {})
            .await
            .map_err(status_to_engine_error)?
            .into_inner();
        Ok(resp.models.into_iter().map(Into::into).collect())
    }

    async fn describe_semantic_model(
        &self,
        name: &str,
    ) -> Result<SemanticModelDescription, EngineError> {
        let mut client = self.semantic.clone();
        let resp = client
            .describe_model(DescribeModelRequest {
                name: name.to_string(),
            })
            .await
            .map_err(status_to_engine_error)?
            .into_inner();
        resp.model
            .ok_or_else(|| EngineError::Protocol("DescribeModelResponse missing model".into()))?
            .try_into()
            .map_err(|e: pawrly_proto::conv::ConvError| EngineError::Protocol(e.to_string()))
    }

    async fn semantic_query(&self, q: SemanticQuery) -> Result<QueryHandle, EngineError> {
        let mut client = self.semantic.clone();
        let proto: v1::SemanticQueryRequest = q.into();
        let mut server_stream = client
            .semantic_query(proto)
            .await
            .map_err(status_to_engine_error)?
            .into_inner();

        let query_id = read_started_id(&mut server_stream).await?;
        let completion: QueryCompletion = Arc::new(std::sync::OnceLock::new());
        let completion_slot = completion.clone();

        let stream = async_stream::try_stream! {
            while let Some(frame) = server_stream.next().await {
                let frame = frame.map_err(status_to_engine_error)?;
                let Some(payload) = frame.payload else { continue; };
                match payload {
                    QueryPayload::Started(_) => continue,
                    QueryPayload::IpcStream(bytes) => {
                        let batches = decode_frame(&bytes)
                            .map_err(|e| EngineError::Protocol(format!("ipc decode: {e}")))?;
                        for b in batches {
                            yield b;
                        }
                    }
                    QueryPayload::Completed(c) => {
                        let _ = completion_slot.set(proto_completed(&c));
                    }
                    QueryPayload::Error(err) => {
                        let mut s = tonic::Status::internal(err.message.clone());
                        if let Ok(v) = err.code.parse() {
                            s.metadata_mut().insert("pawrly-error-code", v);
                        }
                        Err(status_to_engine_error(s))?;
                    }
                }
            }
        };

        Ok(QueryHandle::new(query_id, Box::pin(stream), completion))
    }

    async fn health(&self) -> Result<HealthReport, EngineError> {
        let mut client = self.admin.clone();
        let resp = client
            .health(HealthRequest {})
            .await
            .map_err(status_to_engine_error)?
            .into_inner();
        Ok(resp.into())
    }

    async fn shutdown(&self) -> Result<(), EngineError> {
        // Returning Ok is intentional — `pawrly stop` uses SIGTERM, not RPC.
        Ok(())
    }
}

// Suppress unused-import warning if engine_error_to_status isn't referenced
// on the client side directly.
#[doc(hidden)]
fn _ensure_export() {
    let _ = engine_error_to_status;
}
