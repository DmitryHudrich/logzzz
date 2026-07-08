use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use domain::graph::GraphRequest;
use domain::ports::{BlockRange, IngestionPort};
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ErrorResponse};
use crate::format::parse_address;
use crate::state::{AppState, resolve_chain_id};

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct IngestJobRequest {
    #[schema(example = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")]
    address: String,
    #[schema(example = 1)]
    chain_id: Option<u32>,
    #[schema(example = 3)]
    max_depth: Option<u32>,
    #[schema(example = 500)]
    max_nodes: Option<usize>,
    #[schema(example = 10000)]
    max_transfers_per_address: Option<usize>,
    from_block: Option<u64>,
    to_block: Option<u64>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct JobAcceptedResponse {
    job_id: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct JobStatusResponse {
    id: String,
    status: String,
    error: Option<String>,
    created_at: String,
    updated_at: String,
}

#[utoipa::path(
    post, path = "/jobs/ingest",
    request_body = IngestJobRequest,
    responses(
        (status = 202, body = JobAcceptedResponse),
        (status = 400, body = ErrorResponse),
        (status = 500, body = ErrorResponse),
    ),
    tag = "Jobs"
)]
pub async fn create_ingest_job(
    State(state): State<Arc<AppState>>,
    Json(body): Json<IngestJobRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let chain = resolve_chain_id(&state, body.chain_id);
    let addr = parse_address(&body.address, chain)?;

    let max_depth = body.max_depth.unwrap_or(3);
    let max_nodes = body.max_nodes.unwrap_or(500);
    let max_transfers_per_address = body.max_transfers_per_address.unwrap_or(10_000);
    let range = match (body.from_block, body.to_block) {
        (None, None) => None,
        (from, to) => Some(BlockRange::new(from.unwrap_or(0), to.unwrap_or(u64::MAX))),
    };

    let payload = serde_json::to_value(&body).map_err(|e| ApiError::InternalMsg(e.to_string()))?;

    let job_id = state
        .jobs()
        .create_job("ingest", payload)
        .await
        .map_err(ApiError::Internal)?;

    let state_for_job = Arc::clone(&state);
    let addr_for_job = addr.clone();
    tokio::spawn(async move {
        if let Err(e) = state_for_job.jobs().set_running(job_id).await {
            tracing::error!(error = %e, job_id = %job_id, "failed to mark job running");
            return;
        }
        let req = GraphRequest::new(range, max_depth, max_nodes, max_transfers_per_address);
        match state_for_job
            .ingestion()
            .build_graph(&addr_for_job, req)
            .await
        {
            Ok(_) => {
                if let Err(e) = state_for_job.jobs().set_done(job_id).await {
                    tracing::error!(error = %e, job_id = %job_id, "failed to mark job done");
                }
            }
            Err(e) => {
                let msg = e.to_string();
                tracing::warn!(error = %msg, job_id = %job_id, "ingest job failed");
                if let Err(e2) = state_for_job.jobs().set_failed(job_id, &msg).await {
                    tracing::error!(error = %e2, job_id = %job_id, "failed to mark job failed");
                }
            }
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(JobAcceptedResponse {
            job_id: job_id.to_string(),
        }),
    ))
}

#[utoipa::path(
    get, path = "/jobs/{id}",
    params(("id" = String, Path, description = "Job UUID")),
    responses(
        (status = 200, body = JobStatusResponse),
        (status = 404, body = ErrorResponse),
        (status = 500, body = ErrorResponse),
    ),
    tag = "Jobs"
)]
pub async fn get_job_status(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let uuid = uuid::Uuid::parse_str(&id)
        .map_err(|e| ApiError::bad_request(format!("invalid job id: {e}")))?;

    let job = state
        .jobs()
        .get_job(uuid)
        .await
        .map_err(ApiError::Internal)?;

    let Some(job) = job else {
        return Err(ApiError::BadRequest("job not found".into()));
    };

    Ok(Json(JobStatusResponse {
        id: job.id.to_string(),
        status: job.status,
        error: job.error,
        created_at: job.created_at.to_rfc3339(),
        updated_at: job.updated_at.to_rfc3339(),
    }))
}
