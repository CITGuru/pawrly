//! QueryService implementation.

use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;
use pawrly_core::{EngineService, QueryId, QueryRequest};
use pawrly_proto::arrow_helpers::encode_batch;
use pawrly_proto::v1::{
    self, CancelRequest, CancelResponse, ExplainRequest, ExplainResponse,
    QueryRequest as ProtoQueryRequest, QueryResponse, query_response::Payload,
    query_service_server::QueryService,
};
use prost_types::Timestamp;
use tonic::{Request, Response, Status, async_trait};

use crate::error::engine_error_to_status;

pub(crate) struct QuerySvc {
    engine: Arc<dyn EngineService>,
}

impl QuerySvc {
    pub(crate) fn new(engine: Arc<dyn EngineService>) -> Self {
        Self { engine }
    }
}

type ResponseStream = Pin<Box<dyn Stream<Item = Result<QueryResponse, Status>> + Send>>;

#[async_trait]
impl QueryService for QuerySvc {
    type QueryStream = ResponseStream;

    async fn query(
        &self,
        req: Request<ProtoQueryRequest>,
    ) -> Result<Response<Self::QueryStream>, Status> {
        let core_req: QueryRequest = req.into_inner().into();
        let query_id = QueryId::new(uuid::Uuid::new_v4().to_string());

        let started = QueryResponse {
            payload: Some(Payload::Started(v1::QueryStarted {
                query_id: query_id.0.clone(),
                started_at: Some(now_timestamp()),
            })),
        };

        let inner_stream = match self.engine.query(core_req).await {
            Ok(s) => s,
            Err(e) => return Err(engine_error_to_status(&e)),
        };

        let stream = async_stream::try_stream! {
            yield started;
            futures::pin_mut!(inner_stream);

            use futures_util::StreamExt as _;
            let mut row_count: u64 = 0;
            let started_at = std::time::Instant::now();
            while let Some(item) = inner_stream.next().await {
                match item {
                    Ok(batch) => {
                        row_count += batch.num_rows() as u64;
                        let bytes = encode_batch(&batch).map_err(|e|
                            Status::internal(format!("ipc encode: {e}"))
                        )?;
                        yield QueryResponse {
                            payload: Some(Payload::IpcStream(bytes.to_vec())),
                        };
                    }
                    Err(e) => {
                        let s = engine_error_to_status(&e);
                        yield QueryResponse {
                            payload: Some(Payload::Error(v1::Error {
                                code: s.metadata()
                                    .get("pawrly-error-code")
                                    .and_then(|v| v.to_str().ok())
                                    .unwrap_or("PAWRLY_INTERNAL")
                                    .to_string(),
                                message: e.to_string(),
                                ..Default::default()
                            })),
                        };
                        return;
                    }
                }
            }
            let elapsed = started_at.elapsed();
            yield QueryResponse {
                payload: Some(Payload::Completed(v1::QueryCompleted {
                    rows_returned: row_count,
                    elapsed: Some(prost_types::Duration {
                        seconds: elapsed.as_secs() as i64,
                        nanos: elapsed.subsec_nanos() as i32,
                    }),
                    truncated: false,
                    explain: String::new(),
                })),
            };
        };

        Ok(Response::new(Box::pin(stream) as ResponseStream))
    }

    async fn cancel(
        &self,
        req: Request<CancelRequest>,
    ) -> Result<Response<CancelResponse>, Status> {
        let q = QueryId::new(req.into_inner().query_id);
        let cancelled = self
            .engine
            .cancel(&q)
            .await
            .map_err(|e| engine_error_to_status(&e))?;
        Ok(Response::new(CancelResponse { cancelled }))
    }

    async fn explain(
        &self,
        req: Request<ExplainRequest>,
    ) -> Result<Response<ExplainResponse>, Status> {
        let req = req.into_inner();
        let plan = self
            .engine
            .explain(&req.sql, req.analyze)
            .await
            .map_err(|e| engine_error_to_status(&e))?;
        Ok(Response::new(ExplainResponse { plan }))
    }
}

fn now_timestamp() -> Timestamp {
    let now = chrono::Utc::now();
    Timestamp {
        seconds: now.timestamp(),
        nanos: now.timestamp_subsec_nanos() as i32,
    }
}
