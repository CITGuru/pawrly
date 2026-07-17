//! SemanticService implementation.

use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;
use pawrly_core::EngineService;
use pawrly_core::semantic::SemanticQuery;
use pawrly_proto::arrow_helpers::encode_batch;
use pawrly_proto::v1::{
    self, DescribeMetricRequest, DescribeMetricResponse, DescribeModelRequest,
    DescribeModelResponse, ListMetricsRequest, ListMetricsResponse, ListModelsRequest,
    ListModelsResponse, QueryResponse, SemanticQueryRequest, query_response::Payload,
    semantic_service_server::SemanticService,
};
use prost_types::Timestamp;
use tonic::{Request, Response, Status, async_trait};

use crate::error::engine_error_to_status;

pub(crate) struct SemanticSvc {
    engine: Arc<dyn EngineService>,
}

impl SemanticSvc {
    pub(crate) fn new(engine: Arc<dyn EngineService>) -> Self {
        Self { engine }
    }
}

type ResponseStream = Pin<Box<dyn Stream<Item = Result<QueryResponse, Status>> + Send>>;

#[async_trait]
impl SemanticService for SemanticSvc {
    type SemanticQueryStream = ResponseStream;

    async fn list_models(
        &self,
        _req: Request<ListModelsRequest>,
    ) -> Result<Response<ListModelsResponse>, Status> {
        let models = self
            .engine
            .list_semantic_models()
            .await
            .map_err(|e| engine_error_to_status(&e))?;
        Ok(Response::new(ListModelsResponse {
            models: models.into_iter().map(Into::into).collect(),
        }))
    }

    async fn describe_model(
        &self,
        req: Request<DescribeModelRequest>,
    ) -> Result<Response<DescribeModelResponse>, Status> {
        let name = req.into_inner().name;
        let desc = self
            .engine
            .describe_semantic_model(&name)
            .await
            .map_err(|e| engine_error_to_status(&e))?;
        Ok(Response::new(DescribeModelResponse {
            model: Some(desc.into()),
        }))
    }

    async fn list_metrics(
        &self,
        _req: Request<ListMetricsRequest>,
    ) -> Result<Response<ListMetricsResponse>, Status> {
        let metrics = self
            .engine
            .list_metrics()
            .await
            .map_err(|e| engine_error_to_status(&e))?;
        let metrics_json = metrics
            .iter()
            .map(|m| serde_json::to_vec(m).map_err(|e| Status::internal(e.to_string())))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Response::new(ListMetricsResponse { metrics_json }))
    }

    async fn describe_metric(
        &self,
        req: Request<DescribeMetricRequest>,
    ) -> Result<Response<DescribeMetricResponse>, Status> {
        let name = req.into_inner().name;
        let metric = self
            .engine
            .describe_metric(&name)
            .await
            .map_err(|e| engine_error_to_status(&e))?;
        let metric_json =
            serde_json::to_vec(&metric).map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(DescribeMetricResponse { metric_json }))
    }

    async fn semantic_query(
        &self,
        req: Request<SemanticQueryRequest>,
    ) -> Result<Response<Self::SemanticQueryStream>, Status> {
        let query: SemanticQuery = req.into_inner().into();
        // Call the engine first so the Started frame carries the engine's id —
        // that's what `cancel(id)` routes on.
        let handle = match self.engine.semantic_query(query).await {
            Ok(h) => h,
            Err(e) => return Err(engine_error_to_status(&e)),
        };
        let query_id = handle.id.0.clone();
        let completion = handle.completion;
        let inner_stream = handle.stream;

        let started = QueryResponse {
            payload: Some(Payload::Started(v1::QueryStarted {
                query_id,
                started_at: Some(now_timestamp()),
            })),
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
            let truncated = completion.get().map(|c| c.truncated).unwrap_or(false);
            yield QueryResponse {
                payload: Some(Payload::Completed(v1::QueryCompleted {
                    rows_returned: row_count,
                    elapsed: Some(prost_types::Duration {
                        seconds: elapsed.as_secs() as i64,
                        nanos: elapsed.subsec_nanos() as i32,
                    }),
                    truncated,
                    explain: String::new(),
                })),
            };
        };

        Ok(Response::new(Box::pin(stream) as ResponseStream))
    }
}

fn now_timestamp() -> Timestamp {
    let now = chrono::Utc::now();
    Timestamp {
        seconds: now.timestamp(),
        nanos: now.timestamp_subsec_nanos() as i32,
    }
}
