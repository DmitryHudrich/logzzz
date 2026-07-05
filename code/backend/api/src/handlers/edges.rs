use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ErrorResponse};
use crate::format::parse_address;
use crate::state::{AppState, resolve_chain_id};

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EdgeSignificanceQuery {
    #[param(example = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")]
    address: String,
    #[param(example = 1)]
    chain_id: Option<u32>,
    #[param(example = 50)]
    limit: Option<usize>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct EdgeScoreDto {
    tx_hash: String,
    from: String,
    to: String,
    amount: String,
    asset: String,
    usd_value: Option<f64>,
    timestamp: String,
    score: f64,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct EdgeSignificanceResponse {
    address: String,
    edges: Vec<EdgeScoreDto>,
}

#[utoipa::path(
    get, path = "/edges/significance",
    description = "Rank the transfers around an address by forensic significance.\n\n\
                   ## What this does\n\n\
                   Loads every incoming and outgoing transfer for `address` from the persisted \
                   graph, scores each one with the edge-significance heuristic (which considers \
                   amount, timing, counterparty rarity, and contextual outliers among the \
                   subject's other edges), and returns the top `limit` highest-scoring edges.\n\n\
                   This is a DB-only read. It is a complement to `/heuristics` and `/cluster`: \
                   where those summarise patterns, this surfaces the specific transactions an \
                   investigator should look at first.\n\n\
                   ## When to use it\n\n\
                   Use this as the second step in manual triage of an address: get the score \
                   with `/score`, then pull the most significant edges here to ground the \
                   investigation in concrete transactions. Run `POST /jobs/ingest` first.\n\n\
                   ## Example\n\n\
                   ```bash\n\
                   curl 'http://localhost:8080/edges/significance?address=0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045&chain_id=1&limit=20' \\\n\
                     -H 'X-API-Version: 1'\n\
                   ```\n\n\
                   ## Notes\n\n\
                   The score is heuristic and not directly comparable across different subject \
                   addresses — it is calibrated to the subject's own neighbourhood. `limit` \
                   defaults to 50.",
    params(EdgeSignificanceQuery),
    responses(
        (status = 200, body = EdgeSignificanceResponse),
        (status = 400, body = ErrorResponse),
        (status = 500, body = ErrorResponse),
    ),
    tag = "Risk"
)]
pub async fn edge_significance_endpoint(
    State(state): State<Arc<AppState>>,
    Query(q): Query<EdgeSignificanceQuery>,
) -> Result<impl IntoResponse, ApiError> {
    use domain::ports::TransferRepository;
    let chain = resolve_chain_id(&state, q.chain_id);
    let addr = parse_address(&q.address, chain)?;
    let limit = q.limit.unwrap_or(50);

    let repo = state.ingestion().repo();
    let mut all = repo
        .find_outgoing(&addr, None)
        .await
        .map_err(ApiError::Internal)?;
    all.extend(
        repo.find_incoming(&addr, None)
            .await
            .map_err(ApiError::Internal)?,
    );

    let context = all.clone();
    let mut scored: Vec<(f64, domain::transfer::Transfer)> = all
        .into_iter()
        .map(|t| (usecase::risk::edge_significance(&t, &context), t))
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);

    let edges = scored
        .into_iter()
        .map(|(score, t)| EdgeScoreDto {
            tx_hash: hex::encode(t.tx_ref().hash()),
            from: t.from().canonical(),
            to: t.to().canonical(),
            amount: t.amount().raw().to_string(),
            asset: format!("{}", t.asset()),
            usd_value: t.usd_value().map(|v| v.value()),
            timestamp: t.timestamp().to_rfc3339(),
            score,
        })
        .collect();

    Ok(Json(EdgeSignificanceResponse {
        address: addr.canonical(),
        edges,
    }))
}
