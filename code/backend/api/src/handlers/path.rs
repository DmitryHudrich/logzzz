use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
    response::IntoResponse,
};
use domain::graph::GraphRequest;
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ErrorResponse};
use crate::format::parse_address;
use crate::state::{AppState, resolve_chain_id};

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PathQuery {
    #[param(example = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")]
    from: String,
    #[param(example = "0x742d35Cc6634C0532925a3b844Bc9e7595f0bEb1")]
    to: String,
    #[param(example = 1)]
    chain_id: Option<u32>,
    #[param(example = 3)]
    max_depth: Option<u32>,
    #[param(example = 500)]
    max_nodes: Option<usize>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct PathEdgeDto {
    tx_hash: String,
    from: String,
    to: String,
    amount: String,
    asset: String,
    usd_value: Option<f64>,
    timestamp: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct PathResponse {
    length: usize,
    not_found: bool,
    edges: Vec<PathEdgeDto>,
}

fn edge_dto(t: &domain::transfer::Transfer) -> PathEdgeDto {
    PathEdgeDto {
        tx_hash: hex::encode(t.tx_ref().hash()),
        from: t.from().canonical(),
        to: t.to().canonical(),
        amount: t.amount().raw().to_string(),
        asset: format!("{}", t.asset()),
        usd_value: t.usd_value().map(|v| v.value()),
        timestamp: t.timestamp().to_rfc3339(),
    }
}

#[utoipa::path(
    get, path = "/path",
    description = "Find the shortest transfer chain between two addresses inside the persisted graph.\n\n\
                   ## What this does\n\n\
                   Builds a bounded BFS subgraph anchored at `from` (up to `max_depth` hops and \
                   `max_nodes` distinct addresses), then runs a shortest-path search ending at \
                   `to`. Returns the sequence of transfer edges that connects the two addresses, \
                   or an empty `not_found` response when no such chain exists inside the budget.\n\n\
                   The subgraph is read only from PostgreSQL; this endpoint never contacts a chain \
                   RPC. The path is shortest by hop count — it is not weighted by amount, time, or \
                   risk.\n\n\
                   ## When to use it\n\n\
                   Useful for AML investigations of the form \"is there a chain of transfers from \
                   wallet A to wallet B within N hops?\". Run `POST /jobs/ingest` for `from` first; \
                   if the chain you are looking for fans out wide, also pre-ingest `to` and bump \
                   `max_depth` / `max_nodes`.\n\n\
                   ## Example\n\n\
                   ```bash\n\
                   curl 'http://localhost:8080/path?from=0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045&to=0x742d35Cc6634C0532925a3b844Bc9e7595f0bEb1&chain_id=1&max_depth=4' \\\n\
                     -H 'X-API-Version: 1'\n\
                   ```\n\n\
                   ## Notes\n\n\
                   A `not_found: true` response does NOT prove the addresses are disconnected on \
                   chain — it only means no path was reachable inside the `max_depth` / `max_nodes` \
                   budget you set. Increasing the budget can change the answer.",
    params(PathQuery),
    responses(
        (status = 200, body = PathResponse),
        (status = 400, body = ErrorResponse),
        (status = 500, body = ErrorResponse),
    ),
    tag = "Graph"
)]
pub async fn shortest_path(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PathQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let chain = resolve_chain_id(&state, q.chain_id);
    let a = parse_address(&q.from, chain)?;
    let b = parse_address(&q.to, chain)?;
    let max_depth = q.max_depth.unwrap_or(3);
    let max_nodes = q.max_nodes.unwrap_or(500);

    let graph = state
        .ingestion()
        .build_graph_from_db(
            &a,
            GraphRequest::new(None, max_depth, max_nodes, 10_000),
        )
        .await
        .map_err(ApiError::Internal)?;

    let path = graph.shortest_path(&a, &b);
    let (not_found, edges) = match path {
        Some(es) => (false, es.iter().map(edge_dto).collect()),
        None => (true, Vec::new()),
    };
    let length = if not_found { 0 } else { edges_count(&edges) };
    Ok(Json(PathResponse { length, not_found, edges }))
}

fn edges_count<T>(v: &[T]) -> usize {
    v.len()
}
