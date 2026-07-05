use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
    response::IntoResponse,
};
use domain::ports::RiskPort;
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ErrorResponse};
use crate::format::{parse_address, signal_kind_str};
use crate::state::{AppState, resolve_chain_id};

#[derive(Serialize, utoipa::ToSchema)]
pub struct SignalDto {
    kind: String,
    severity: u8,
    description: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ScoreResponse {
    address: String,
    chain_id: u32,
    score: u8,
    is_high_risk: bool,
    signals: Vec<SignalDto>,
    generated_at: String,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ScoreQuery {
    #[param(example = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")]
    address: String,
    #[param(example = 1)]
    chain_id: Option<u32>,
}

#[utoipa::path(
    get, path = "/score",
    description = "Compute a 0–100 risk score for an address with explainable signals.\n\n\
                   ## What this does\n\n\
                   Aggregates risk signals attached to the address into a single 0–100 score. \
                   Inputs are entity labels (from `/labels`), built-in sanctions lists, and the \
                   sinks discovered by an internal forward + backward Haircut trace over the \
                   persisted graph. Signals are combined using the strategy configured under \
                   the `score:` block in `config.yaml` (`max` by default, or `weighted_count` \
                   with deduplication).\n\n\
                   This is a DB-only read — the chain RPC is never touched. If you score an \
                   address that has not been ingested, you will get a low score even when the \
                   real on-chain reality says otherwise: ingest first.\n\n\
                   ## When to use it\n\n\
                   After running `POST /jobs/ingest` for the address, call this whenever you \
                   need a single risk number plus explainable evidence. Surface the `signals` \
                   array verbatim in your UI so investigators can audit each contributor.\n\n\
                   ## Example\n\n\
                   ```bash\n\
                   curl 'http://localhost:8080/score?address=0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045&chain_id=1' \\\n\
                     -H 'X-API-Version: 1'\n\
                   ```\n\n\
                   ## Notes\n\n\
                   The score is cached for a short, configurable TTL (`risk_cache:` block in \
                   `config.yaml`), so back-to-back calls return the same result without redoing \
                   the trace.",
    params(ScoreQuery),
    responses(
        (status = 200, body = ScoreResponse),
        (status = 400, body = ErrorResponse),
        (status = 500, body = ErrorResponse),
    ),
    tag = "Risk"
)]
pub async fn score_address(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ScoreQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let chain = resolve_chain_id(&state, q.chain_id);
    let addr = parse_address(&q.address, chain)?;

    let report = state
        .risk()
        .score(&addr)
        .await
        .map_err(ApiError::Internal)?;

    Ok(Json(report_to_dto(&report)))
}

pub(crate) fn report_to_dto(report: &domain::risk::RiskReport) -> ScoreResponse {
    let signals: Vec<SignalDto> = report
        .signals()
        .iter()
        .map(|s| SignalDto {
            kind: signal_kind_str(s.kind()).into(),
            severity: s.severity().value(),
            description: s.description().to_string(),
        })
        .collect();

    ScoreResponse {
        address: report.subject().canonical(),
        chain_id: report.subject().chain().value(),
        score: report.overall_score().value(),
        is_high_risk: report.is_high_risk(),
        signals,
        generated_at: report.generated_at().to_rfc3339(),
    }
}
