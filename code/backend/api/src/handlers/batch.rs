use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    response::IntoResponse,
};
use domain::ports::RiskPort;
use domain::primitives::Address;
use serde::Deserialize;

use crate::error::{ApiError, ErrorResponse};
use crate::format::parse_address;
use crate::handlers::sanctions::{SanctionsResponse, sanctions_to_dto};
use crate::handlers::score::{ScoreResponse, report_to_dto};
use crate::state::{AppState, resolve_chain_id};

#[derive(Deserialize, utoipa::ToSchema)]
pub struct BatchItem {
    #[schema(example = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")]
    address: String,
    #[schema(example = 1)]
    chain_id: Option<u32>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct SanctionsBatchRequest {
    items: Vec<BatchItem>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ScoreBatchRequest {
    items: Vec<BatchItem>,
}

const MAX_SANCTIONS_BATCH: usize = 100;
const MAX_SCORE_BATCH: usize = 50;

#[utoipa::path(
    post, path = "/sanctions/batch",
    request_body = SanctionsBatchRequest,
    responses(
        (status = 200, body = [SanctionsResponse]),
        (status = 400, body = ErrorResponse),
        (status = 500, body = ErrorResponse),
    ),
    tag = "Risk"
)]
pub async fn sanctions_batch(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SanctionsBatchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    if body.items.len() > MAX_SANCTIONS_BATCH {
        return Err(ApiError::bad_request(format!(
            "batch too large: {} > {MAX_SANCTIONS_BATCH}",
            body.items.len()
        )));
    }

    let mut addrs: Vec<Address> = Vec::with_capacity(body.items.len());
    for it in &body.items {
        let chain = resolve_chain_id(&state, it.chain_id);
        addrs.push(parse_address(&it.address, chain)?);
    }

    let results = state
        .risk()
        .check_sanctions_batch(&addrs)
        .await
        .map_err(ApiError::Internal)?;

    let dtos: Vec<SanctionsResponse> = results.iter().map(sanctions_to_dto).collect();
    Ok(Json(dtos))
}

#[utoipa::path(
    post, path = "/score/batch",
    request_body = ScoreBatchRequest,
    responses(
        (status = 200, body = [ScoreResponse]),
        (status = 400, body = ErrorResponse),
        (status = 500, body = ErrorResponse),
    ),
    tag = "Risk"
)]
pub async fn score_batch(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ScoreBatchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    if body.items.len() > MAX_SCORE_BATCH {
        return Err(ApiError::bad_request(format!(
            "batch too large: {} > {MAX_SCORE_BATCH}",
            body.items.len()
        )));
    }

    let mut addrs: Vec<Address> = Vec::with_capacity(body.items.len());
    for it in &body.items {
        let chain = resolve_chain_id(&state, it.chain_id);
        addrs.push(parse_address(&it.address, chain)?);
    }

    let reports = state
        .risk()
        .score_batch(&addrs)
        .await
        .map_err(ApiError::Internal)?;

    let dtos: Vec<ScoreResponse> = reports.iter().map(report_to_dto).collect();
    Ok(Json(dtos))
}
