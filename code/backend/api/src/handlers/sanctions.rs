use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
    response::IntoResponse,
};
use domain::ports::RiskPort;
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ErrorResponse};
use crate::format::{parse_address, sanction_list_str};
use crate::state::{AppState, resolve_chain_id};

#[derive(Serialize, utoipa::ToSchema)]
pub struct SanctionsResponse {
    address: String,
    chain_id: u32,
    is_sanctioned: bool,
    sanction_list: Option<String>,
    label: Option<String>,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SanctionsQuery {
    #[param(example = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")]
    address: String,
    #[param(example = 1)]
    chain_id: Option<u32>,
}

#[utoipa::path(
    get, path = "/sanctions",
    description = "Check whether an address is on any configured sanctions list.\n\n\
                   ## What this does\n\n\
                   Looks the address up against every sanctions list known to the server (OFAC, \
                   EU, UN, plus any custom lists you have imported via `/labels` with \
                   `category: sanctioned`). Returns a boolean plus, when matched, which list and \
                   the entity label.\n\n\
                   This is a fast in-memory + DB read; no chain RPC is touched. Sanctions lists \
                   are loaded from the entity store on startup and refreshed whenever you POST \
                   to `/labels`, so the result is always consistent with the latest admin \
                   imports.\n\n\
                   ## When to use it\n\n\
                   Suitable as a compliance gate before accepting an inbound deposit or before \
                   sending funds to a counterparty. For high-volume use cases, prefer \
                   `POST /sanctions/batch` to amortise round-trips.\n\n\
                   ## Example\n\n\
                   ```bash\n\
                   curl 'http://localhost:8080/sanctions?address=0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045&chain_id=1' \\\n\
                     -H 'X-API-Version: 1'\n\
                   ```\n\n\
                   ## Notes\n\n\
                   A `false` here only means the exact address is not directly listed. It does \
                   NOT prove the address has no exposure to sanctioned funds — use `/score` or \
                   `/trace` for indirect exposure.",
    params(SanctionsQuery),
    responses(
        (status = 200, body = SanctionsResponse),
        (status = 400, body = ErrorResponse),
        (status = 500, body = ErrorResponse),
    ),
    tag = "Risk"
)]
pub async fn check_sanctions(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SanctionsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let chain = resolve_chain_id(&state, q.chain_id);
    let addr = parse_address(&q.address, chain)?;

    let result = state
        .risk()
        .check_sanctions(&addr)
        .await
        .map_err(ApiError::Internal)?;

    Ok(Json(sanctions_to_dto(&result)))
}

pub(crate) fn sanctions_to_dto(result: &domain::risk::SanctionsCheckResult) -> SanctionsResponse {
    SanctionsResponse {
        address: result.address().canonical(),
        chain_id: result.address().chain().value(),
        is_sanctioned: result.is_sanctioned(),
        sanction_list: result.sanction_list().map(sanction_list_str),
        label: result.label().map(str::to_string),
    }
}
