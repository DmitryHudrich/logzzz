use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ErrorResponse};
use crate::format::parse_address;
use crate::handlers::labels::PathAddrQuery;
use crate::state::{AppState, resolve_chain_id};

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct AddressKindRequest {
    #[schema(example = "contract")]
    kind: String,
    #[schema(example = "Binance hot wallet")]
    service_name: Option<String>,
    #[schema(example = 1)]
    chain_id: Option<u32>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct AddressKindResponse {
    address: String,
    kind: String,
    service_name: Option<String>,
}

#[utoipa::path(
    get, path = "/address/{addr}/kind",
    description = "Look up the address-kind classification (EOA / Contract / KnownService).\n\n\
                   ## What this does\n\n\
                   Returns whatever classification the server currently holds for the address. \
                   The kind is set either by the ingestion classifier (which detects EOA vs. \
                   contract from on-chain code) or by an explicit admin override via \
                   `POST /address/{addr}/kind`. Addresses the classifier has never seen come \
                   back as `unknown`.\n\n\
                   This is a fast DB-only read; no chain RPC is touched.\n\n\
                   ## When to use it\n\n\
                   Use this when rendering an address in a UI to pick the right icon (wallet \
                   vs. contract vs. named service), or to gate features that only make sense \
                   for EOAs.\n\n\
                   ## Example\n\n\
                   ```bash\n\
                   curl 'http://localhost:8080/address/0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045/kind' \\\n\
                     -H 'X-API-Version: 1'\n\
                   ```\n\n\
                   ## Notes\n\n\
                   `chain_id` defaults to the server-wide `api.default_chain_id` (typically 1 = \
                   Ethereum mainnet). Pass it explicitly for non-EVM addresses so the parser \
                   picks the right encoding.",
    params(
        ("addr" = String, Path, description = "Address in hex (EVM) or canonical chain form (other families)."),
        PathAddrQuery,
    ),
    responses(
        (status = 200, body = AddressKindResponse),
        (status = 400, body = ErrorResponse),
    ),
    tag = "Risk"
)]
pub async fn get_address_kind(
    State(state): State<Arc<AppState>>,
    Path(addr_param): Path<String>,
    Query(q): Query<PathAddrQuery>,
) -> Result<impl IntoResponse, ApiError> {
    use domain::ports::AddressKindRepository;
    let chain = resolve_chain_id(&state, q.chain_id);
    let addr = parse_address(&addr_param, chain)?;
    let kind = state.address_kinds().kind(&addr).await.map_err(ApiError::Internal)?;
    let (kind_str, name) = match kind {
        domain::entity::AddressKind::Eoa => ("eoa".to_string(), None),
        domain::entity::AddressKind::Contract => ("contract".to_string(), None),
        domain::entity::AddressKind::KnownService(n) => ("known_service".to_string(), Some(n)),
        domain::entity::AddressKind::Unknown => ("unknown".to_string(), None),
    };
    Ok(Json(AddressKindResponse { address: addr.canonical(), kind: kind_str, service_name: name }))
}

#[utoipa::path(
    post, path = "/address/{addr}/kind",
    description = "Override the address-kind classification for an address (admin only).\n\n\
                   ## What this does\n\n\
                   Writes the supplied kind/service_name pair into the address-kinds table, \
                   replacing any prior classification. Useful when the automatic classifier has \
                   nothing to go on (e.g. an account that has not yet emitted code) or when you \
                   want to attach a friendly service name (`known_service`).\n\n\
                   The new value takes effect immediately for all subsequent reads — the \
                   classifier honours admin overrides.\n\n\
                   ## When to use it\n\n\
                   Use this for curated allow/deny lists of named services (exchanges, payment \
                   processors). For pure risk labels (mixer / sanctioned / scam), prefer \
                   `POST /labels` — the categorisation there carries a richer model.\n\n\
                   ## Example\n\n\
                   ```bash\n\
                   curl -X POST 'http://localhost:8080/address/0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045/kind' \\\n\
                     -H 'X-API-Version: 1' \\\n\
                     -H 'X-Admin-Api-Key: <admin-key>' \\\n\
                     -H 'Content-Type: application/json' \\\n\
                     -d '{\"kind\": \"known_service\", \"service_name\": \"Binance hot wallet\", \"chain_id\": 1}'\n\
                   ```\n\n\
                   ## Notes\n\n\
                   Requires the `X-Admin-Api-Key` header. `kind` must be one of `eoa`, \
                   `contract`, `known_service`, `unknown` — anything else triggers a 400.",
    params(("addr" = String, Path, description = "Address in hex (EVM) or canonical chain form.")),
    request_body = AddressKindRequest,
    responses(
        (status = 200, body = AddressKindResponse),
        (status = 400, body = ErrorResponse),
        (status = 401, body = ErrorResponse),
    ),
    tag = "Risk"
)]
pub async fn set_address_kind(
    State(state): State<Arc<AppState>>,
    Path(addr_param): Path<String>,
    Json(body): Json<AddressKindRequest>,
) -> Result<impl IntoResponse, ApiError> {
    use domain::ports::AddressKindRepository;
    let chain = resolve_chain_id(&state, body.chain_id);
    let addr = parse_address(&addr_param, chain)?;
    let kind = match body.kind.as_str() {
        "eoa" => domain::entity::AddressKind::Eoa,
        "contract" => domain::entity::AddressKind::Contract,
        "known_service" => domain::entity::AddressKind::KnownService(
            body.service_name.clone().unwrap_or_default(),
        ),
        "unknown" => domain::entity::AddressKind::Unknown,
        other => {
            return Err(ApiError::BadRequest(format!(
                "unknown kind '{other}' (allowed: eoa, contract, known_service, unknown)"
            )));
        }
    };
    state
        .address_kinds()
        .set_kind(&addr, kind.clone())
        .await
        .map_err(ApiError::Internal)?;
    let (kind_str, name) = match kind {
        domain::entity::AddressKind::Eoa => ("eoa".to_string(), None),
        domain::entity::AddressKind::Contract => ("contract".to_string(), None),
        domain::entity::AddressKind::KnownService(n) => ("known_service".to_string(), Some(n)),
        domain::entity::AddressKind::Unknown => ("unknown".to_string(), None),
    };
    Ok(Json(AddressKindResponse { address: addr.canonical(), kind: kind_str, service_name: name }))
}
