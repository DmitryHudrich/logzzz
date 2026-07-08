use std::sync::Arc;

use axum::{Json, extract::State, response::IntoResponse};
use domain::chain::ChainId;
use serde::Serialize;

use crate::state::AppState;

#[derive(Serialize, utoipa::ToSchema)]
pub struct ChainDto {
    id: u32,
    name: String,
    family: String,
    address_model: String,
    address_encoding: String,
    native_symbol: String,
    native_decimals: u8,
    confirmation_depth: u64,
    source_registered: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ChainsResponse {
    chains: Vec<ChainDto>,
}

#[utoipa::path(
    get, path = "/chains",
    description = "List every blockchain the server knows about and whether it is operational.\n\n\
                   ## What this does\n\n\
                   Returns the full chain registry — one entry per supported chain — together \
                   with a flag (`source_registered`) indicating whether a live data source is \
                   configured for that chain on this server. Use this endpoint to discover \
                   which `chain_id` values are valid in other endpoints and which of those \
                   chains can actually serve traffic right now.\n\n\
                   This is a pure in-memory read (no DB, no chain RPC), so it is cheap to \
                   call from a client at startup or for health-style checks.\n\n\
                   ## When to use it\n\n\
                   Call this once at client startup to populate a chain picker, or whenever \
                   you receive a `400` complaining about an unknown chain id. A chain with \
                   `source_registered = false` is in the registry but cannot ingest or read \
                   data — calls passing its `chain_id` will fail.\n\n\
                   ## Example\n\n\
                   ```bash\n\
                   curl 'http://localhost:8080/chains' \\\n\
                     -H 'X-API-Version: 1'\n\
                   ```\n\n\
                   ## Notes\n\n\
                   The set of registered chains is fixed for the lifetime of the server \
                   process and is driven by `config.yaml` (the presence of `ethereum:` / \
                   `tron:` blocks and configured source credentials).",
    responses(
        (status = 200, body = ChainsResponse),
    ),
    tag = "Discovery"
)]
pub async fn list_chains(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    use domain::chain::{AddressModel, ChainFamily};
    let registered: std::collections::HashSet<ChainId> = {
        use domain::ports::ChainSourceRegistry;
        state
            .ingestion()
            .sources()
            .supported_chains()
            .into_iter()
            .collect()
    };
    let chains = state
        .chains()
        .all()
        .iter()
        .map(|m| ChainDto {
            id: m.id().value(),
            name: m.name().to_string(),
            family: match m.family() {
                ChainFamily::Evm => "evm",
                ChainFamily::Tron => "tron",
                ChainFamily::Bitcoin => "bitcoin",
                ChainFamily::Solana => "solana",
                ChainFamily::Other => "other",
            }
            .into(),
            address_model: match m.address_model() {
                AddressModel::Account => "account",
                AddressModel::Utxo => "utxo",
            }
            .into(),
            address_encoding: m.address_encoding().wire_name().into(),
            native_symbol: m.native_asset_symbol().to_string(),
            native_decimals: m.native_asset_decimals(),
            confirmation_depth: m.confirmation_depth(),
            source_registered: registered.contains(&m.id()),
        })
        .collect();
    Json(ChainsResponse { chains })
}
