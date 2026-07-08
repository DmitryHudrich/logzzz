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
pub struct ClusterQuery {
    #[param(example = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")]
    address: String,
    #[param(example = 1)]
    chain_id: Option<u32>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ClusterResponse {
    address: String,
    components: Vec<Vec<String>>,
}

#[utoipa::path(
    get, path = "/cluster",
    description = "Group an address with its likely co-owned siblings using Union-Find over every heuristic.\n\n\
                   ## What this does\n\n\
                   Runs every cluster-formation detector (fan-in, fan-out, smurfing cycle, \
                   peeling chain, deposit-address reuse, ...) over the persisted graph around \
                   the address and unions each detector's outputs into connected components \
                   using Union-Find. The result is a partition of the discovered addresses \
                   into clusters that probably share a real-world owner.\n\n\
                   This endpoint reads only from the DB. The queried address is always returned \
                   in `components[0]`; other components describe satellite clusters discovered \
                   during traversal.\n\n\
                   ## When to use it\n\n\
                   Use this after ingesting an address (`POST /jobs/ingest`) to expand a single \
                   suspect into the wider set of addresses that probably belong to the same \
                   actor. Complementary to `/heuristics` (which says *why*) — this endpoint says \
                   *with whom*.\n\n\
                   ## Example\n\n\
                   ```bash\n\
                   curl 'http://localhost:8080/cluster?address=0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045&chain_id=1' \\\n\
                     -H 'X-API-Version: 1'\n\
                   ```\n\n\
                   ## Notes\n\n\
                   Component contents are heuristic — they are an *upper bound* on co-ownership, \
                   not a proof. Larger components on whales/exchanges are expected; treat the \
                   first component as the primary answer.",
    params(ClusterQuery),
    responses(
        (status = 200, body = ClusterResponse),
        (status = 400, body = ErrorResponse),
        (status = 500, body = ErrorResponse),
    ),
    tag = "Risk"
)]
pub async fn cluster_address(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ClusterQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let chain = resolve_chain_id(&state, q.chain_id);
    let addr = parse_address(&q.address, chain)?;

    let mut components = state
        .risk()
        .cluster_address(&addr)
        .await
        .map_err(ApiError::Internal)?;
    components.sort_by_key(|c| std::cmp::Reverse(c.iter().any(|a| a == &addr) as i32));

    Ok(Json(ClusterResponse {
        address: addr.canonical(),
        components: components
            .into_iter()
            .map(|c| c.into_iter().map(|a| a.canonical()).collect())
            .collect(),
    }))
}
