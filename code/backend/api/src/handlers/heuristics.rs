use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
    response::IntoResponse,
};
use domain::ports::RiskPort;
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ErrorResponse};
use crate::format::parse_address;
use crate::state::{AppState, resolve_chain_id};

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct HeuristicsQuery {
    #[param(example = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")]
    address: String,
    #[param(example = 1)]
    chain_id: Option<u32>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct HeuristicEvidenceDto {
    #[schema(example = "FanOut")]
    heuristic: String,
    #[schema(example = "MEDIUM")]
    confidence: String,
    addresses: Vec<String>,
    #[schema(example = "5 unique receivers within a 86400s window from 0x…")]
    notes: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct HeuristicsResponse {
    address: String,
    fan_out: Option<HeuristicEvidenceDto>,
    fan_in: Option<HeuristicEvidenceDto>,
    smurfing_cycle: Option<HeuristicEvidenceDto>,
    temporal_burst: Option<HeuristicEvidenceDto>,
    fixed_amount_clustering: Option<HeuristicEvidenceDto>,
    dwell_time_pass_through: Option<HeuristicEvidenceDto>,
    peeling_chain: Option<HeuristicEvidenceDto>,
    deposit_address_reuse: Option<HeuristicEvidenceDto>,
}

fn evidence_to_dto(
    e: &domain::entity::ClusterEvidence,
) -> HeuristicEvidenceDto {
    HeuristicEvidenceDto {
        heuristic: format!("{:?}", e.heuristic()),
        confidence: format!("{:?}", e.confidence()),
        addresses: e.addresses().iter().map(|a| a.canonical()).collect(),
        notes: e.notes().map(|s| s.to_owned()),
    }
}

#[utoipa::path(
    get, path = "/heuristics",
    description = "Run every behavioural-pattern detector against an address and report what fired.\n\n\
                   ## What this does\n\n\
                   Evaluates eight independent cluster-formation heuristics against the \
                   persisted transfers around `address`: fan-out, fan-in, smurfing cycle, \
                   temporal burst, fixed-amount clustering, dwell-time pass-through, peeling \
                   chain, and deposit-address reuse. Each detector independently returns an \
                   evidence object (with the participating counterparties and a confidence \
                   band) or `null` when its pattern did not match.\n\n\
                   All thresholds — minimum fan-out/fan-in counts, burst multipliers, window \
                   lengths, BFS depth caps — come from the `heuristics:` block in \
                   `config.yaml`. This endpoint is DB-only; no chain RPC is touched.\n\n\
                   ## When to use it\n\n\
                   Use this to explain *why* an address looks suspicious in human terms — the \
                   evidence objects list the actual counterparty addresses and the matched \
                   counts. Run `POST /jobs/ingest` for the address first; without persisted \
                   counterparties, every detector returns `null`.\n\n\
                   ## Example\n\n\
                   ```bash\n\
                   curl 'http://localhost:8080/heuristics?address=0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045&chain_id=1' \\\n\
                     -H 'X-API-Version: 1'\n\
                   ```\n\n\
                   ## Notes\n\n\
                   Heuristics are independent — getting one match does not affect another. \
                   `null` for every field is the most common outcome for ordinary addresses \
                   and is not an error.",
    params(HeuristicsQuery),
    responses(
        (status = 200, body = HeuristicsResponse),
        (status = 400, body = ErrorResponse),
        (status = 500, body = ErrorResponse),
    ),
    tag = "Risk"
)]
pub async fn detect_heuristics(
    State(state): State<Arc<AppState>>,
    Query(q): Query<HeuristicsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let chain = resolve_chain_id(&state, q.chain_id);
    let addr = parse_address(&q.address, chain)?;

    let risk = state.risk();
    let fan_out = risk.detect_fan_out(&addr).await.map_err(ApiError::Internal)?;
    let fan_in = risk.detect_fan_in(&addr).await.map_err(ApiError::Internal)?;
    let smurf = risk
        .detect_smurfing_cycle(&addr)
        .await
        .map_err(ApiError::Internal)?;
    let burst = risk
        .detect_temporal_burst(&addr)
        .await
        .map_err(ApiError::Internal)?;
    let fixed = risk
        .detect_fixed_amount_clustering(&addr)
        .await
        .map_err(ApiError::Internal)?;
    let dwell = risk
        .detect_dwell_time(&addr)
        .await
        .map_err(ApiError::Internal)?;
    let peeling = risk
        .detect_peeling_chain(&addr)
        .await
        .map_err(ApiError::Internal)?;
    let deposit = risk
        .deposit_reuse_cluster(&addr)
        .await
        .map_err(ApiError::Internal)?;

    Ok(Json(HeuristicsResponse {
        address: addr.canonical(),
        fan_out: fan_out.as_ref().map(evidence_to_dto),
        fan_in: fan_in.as_ref().map(evidence_to_dto),
        smurfing_cycle: smurf.as_ref().map(evidence_to_dto),
        temporal_burst: burst.as_ref().map(evidence_to_dto),
        fixed_amount_clustering: fixed.as_ref().map(evidence_to_dto),
        dwell_time_pass_through: dwell.as_ref().map(evidence_to_dto),
        peeling_chain: peeling.as_ref().map(evidence_to_dto),
        deposit_address_reuse: deposit.as_ref().map(evidence_to_dto),
    }))
}
