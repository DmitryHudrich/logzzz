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

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct WatchlistAddRequest {
    #[schema(example = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")]
    address: String,
    #[schema(example = 1)]
    chain_id: Option<u32>,
    #[schema(example = "Suspect address from incident #42")]
    reason: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct WatchlistEntryDto {
    address: String,
    reason: Option<String>,
}

#[utoipa::path(
    post, path = "/watchlist",
    description = "Add an address to the watchlist (admin only).\n\n\
                   ## What this does\n\n\
                   Persists the address in the watchlist table. From this point on, any \
                   ingestion that touches the address — whether it was the explicit ingest \
                   target or just a counterparty discovered during BFS — automatically writes \
                   an entry to `/alerts` recording the triggering transfer.\n\n\
                   This is a DB-only write. The address itself does not need to have been \
                   ingested previously; you can pre-watch an address and the alerts will start \
                   appearing on the next ingestion run.\n\n\
                   ## When to use it\n\n\
                   Use this for proactive monitoring — for example, watch a sanctioned wallet \
                   so any future contact with it shows up in `/alerts` without you having to \
                   re-score every address you ingest.\n\n\
                   ## Example\n\n\
                   ```bash\n\
                   curl -X POST 'http://localhost:8080/watchlist' \\\n\
                     -H 'X-API-Version: 1' \\\n\
                     -H 'X-Admin-Api-Key: <admin-key>' \\\n\
                     -H 'Content-Type: application/json' \\\n\
                     -d '{\"address\": \"0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045\", \"chain_id\": 1, \"reason\": \"incident #42\"}'\n\
                   ```\n\n\
                   ## Notes\n\n\
                   Requires the `X-Admin-Api-Key` header (or the regular `X-Api-Key` when no \
                   admin key is configured). Adding an address that already exists is a no-op.",
    request_body = WatchlistAddRequest,
    responses(
        (status = 200, body = WatchlistEntryDto),
        (status = 400, body = ErrorResponse),
        (status = 401, body = ErrorResponse),
    ),
    tag = "Watchlist"
)]
pub async fn watchlist_add(
    State(state): State<Arc<AppState>>,
    Json(body): Json<WatchlistAddRequest>,
) -> Result<impl IntoResponse, ApiError> {
    use domain::ports::{WatchlistEntry, WatchlistRepository};
    let chain = resolve_chain_id(&state, body.chain_id);
    let addr = parse_address(&body.address, chain)?;
    state
        .watchlist()
        .add(WatchlistEntry {
            address: addr.clone(),
            reason: body.reason.clone(),
        })
        .await
        .map_err(ApiError::Internal)?;
    Ok(Json(WatchlistEntryDto {
        address: addr.canonical(),
        reason: body.reason,
    }))
}

#[utoipa::path(
    get, path = "/watchlist",
    description = "Return the full watchlist (admin only).\n\n\
                   ## What this does\n\n\
                   Reads every entry currently in the watchlist table and returns them as a \
                   flat array. There is no pagination — the watchlist is an admin tool expected \
                   to stay small (typically tens to low hundreds of entries).\n\n\
                   ## When to use it\n\n\
                   Use this to audit which addresses are currently being monitored. Pair with \
                   `GET /alerts` to see which of these have actually triggered.\n\n\
                   ## Example\n\n\
                   ```bash\n\
                   curl 'http://localhost:8080/watchlist' \\\n\
                     -H 'X-API-Version: 1' \\\n\
                     -H 'X-Admin-Api-Key: <admin-key>'\n\
                   ```\n\n\
                   ## Notes\n\n\
                   Requires the `X-Admin-Api-Key` header (or the regular `X-Api-Key` when no \
                   admin key is configured).",
    responses(
        (status = 200, body = [WatchlistEntryDto]),
        (status = 401, body = ErrorResponse),
    ),
    tag = "Watchlist"
)]
pub async fn watchlist_list(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    use domain::ports::WatchlistRepository;
    let entries = state.watchlist().list().await.map_err(ApiError::Internal)?;
    let dto: Vec<WatchlistEntryDto> = entries
        .into_iter()
        .map(|e| WatchlistEntryDto {
            address: e.address.canonical(),
            reason: e.reason,
        })
        .collect();
    Ok(Json(dto))
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct WatchlistRemoveQuery {
    #[param(example = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")]
    address: String,
    #[param(example = 1)]
    chain_id: Option<u32>,
}

#[utoipa::path(
    delete, path = "/watchlist",
    description = "Remove an address from the watchlist (admin only).\n\n\
                   ## What this does\n\n\
                   Deletes the watchlist row for the supplied address, if any. Existing alerts \
                   already in the `/alerts` log are not touched — only future ingestions stop \
                   raising new alerts for this address.\n\n\
                   ## When to use it\n\n\
                   Use this when an incident is closed or a counterparty has been cleared. The \
                   audit trail (existing alerts) is preserved.\n\n\
                   ## Example\n\n\
                   ```bash\n\
                   curl -X DELETE 'http://localhost:8080/watchlist?address=0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045&chain_id=1' \\\n\
                     -H 'X-API-Version: 1' \\\n\
                     -H 'X-Admin-Api-Key: <admin-key>'\n\
                   ```\n\n\
                   ## Notes\n\n\
                   Requires the `X-Admin-Api-Key` header. Removing an address that is not on \
                   the watchlist is not an error — the response indicates whether anything was \
                   actually removed.",
    params(WatchlistRemoveQuery),
    responses(
        (status = 200),
        (status = 400, body = ErrorResponse),
        (status = 401, body = ErrorResponse),
    ),
    tag = "Watchlist"
)]
pub async fn watchlist_remove(
    State(state): State<Arc<AppState>>,
    Query(q): Query<WatchlistRemoveQuery>,
) -> Result<impl IntoResponse, ApiError> {
    use domain::ports::WatchlistRepository;
    let chain = resolve_chain_id(&state, q.chain_id);
    let addr = parse_address(&q.address, chain)?;
    let removed = state.watchlist().remove(&addr).await.map_err(ApiError::Internal)?;
    Ok(Json(serde_json::json!({ "removed": removed })))
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct AlertDto {
    address: String,
    tx_hash: String,
    tx_idx: u32,
    created_at: String,
    reason: Option<String>,
}

#[utoipa::path(
    get, path = "/alerts",
    description = "Return the alert audit log (admin only).\n\n\
                   ## What this does\n\n\
                   Reads every alert previously written by ingestion runs that touched a \
                   watchlisted address. Each entry records the watched address, the specific \
                   triggering transfer, the timestamp, and the watchlist reason as of the \
                   moment the alert was raised.\n\n\
                   This endpoint is the audit-log counterpart to `/watchlist`: the watchlist \
                   defines *what to watch for*, and this endpoint shows *what has happened*.\n\n\
                   ## When to use it\n\n\
                   Poll periodically from a monitoring dashboard, or read on demand when \
                   investigating an incident. Removing an address from `/watchlist` does NOT \
                   remove existing alerts — they remain for audit purposes.\n\n\
                   ## Example\n\n\
                   ```bash\n\
                   curl 'http://localhost:8080/alerts' \\\n\
                     -H 'X-API-Version: 1' \\\n\
                     -H 'X-Admin-Api-Key: <admin-key>'\n\
                   ```\n\n\
                   ## Notes\n\n\
                   Requires the `X-Admin-Api-Key` header. No pagination — the response \
                   includes the full log. If the volume grows large, prune it directly in the \
                   database.",
    responses(
        (status = 200, body = [AlertDto]),
        (status = 401, body = ErrorResponse),
    ),
    tag = "Alerts"
)]
pub async fn list_alerts(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    use domain::ports::AlertSink;
    let alerts = state.alerts().list().await.map_err(ApiError::Internal)?;
    let dto: Vec<AlertDto> = alerts
        .into_iter()
        .map(|a| AlertDto {
            address: a.address.canonical(),
            tx_hash: hex::encode(a.triggered_by_tx),
            tx_idx: a.triggered_by_idx,
            created_at: a.created_at.to_rfc3339(),
            reason: a.reason,
        })
        .collect();
    Ok(Json(dto))
}
