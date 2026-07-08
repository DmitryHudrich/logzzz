use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
    response::IntoResponse,
};
use domain::ports::RiskPort;
use domain::primitives::Ratio;
use domain::trace::{TaintStrategy, TraceDirection, TraceLimits, TraceOrigin, TraceRequest};
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ErrorResponse};
use crate::format::{format_amount, parse_address, sink_kind_str};
use crate::state::{AppState, resolve_chain_id};

#[derive(Serialize, utoipa::ToSchema)]
pub struct TraceStatsDto {
    addresses_visited: usize,
    transfers_evaluated: usize,
    paths_found: usize,
    depth_reached: u32,
    truncated: bool,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct SinkDto {
    address: String,
    kind: String,
    name: Option<String>,
    risk_score: u8,
    tainted_amount: String,
    formatted: String,
    taint_ratio: f64,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct PathDto {
    depth: u32,
    tainted_amount: String,
    taint_ratio: f64,
    hops: usize,
    origin: Option<String>,
    destination: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct TraceResponse {
    stats: TraceStatsDto,
    sinks: Vec<SinkDto>,
    paths: Vec<PathDto>,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TraceQuery {
    #[param(example = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")]
    address: String,
    #[param(example = 1)]
    chain_id: Option<u32>,
    #[param(example = "forward")]
    direction: Option<String>,
    #[param(example = "haircut")]
    strategy: Option<String>,
    #[param(example = 5)]
    max_hops: Option<u32>,
    #[param(example = 500)]
    max_addresses: Option<usize>,
    #[param(example = 0.2)]
    min_significance: Option<f64>,
}

#[utoipa::path(
    get, path = "/trace",
    params(TraceQuery),
    responses(
        (status = 200, body = TraceResponse),
        (status = 400, body = ErrorResponse),
        (status = 500, body = ErrorResponse),
    ),
    tag = "Risk"
)]
pub async fn trace_funds(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TraceQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let chain = resolve_chain_id(&state, q.chain_id);
    let addr = parse_address(&q.address, chain)?;

    let direction = match q.direction.as_deref().unwrap_or("forward") {
        "forward" => TraceDirection::Forward,
        "backward" => TraceDirection::Backward,
        "both" => TraceDirection::Both,
        other => return Err(ApiError::bad_request(format!("unknown direction: {other}"))),
    };

    let strategy = match q.strategy.as_deref().unwrap_or("haircut") {
        "haircut" => TaintStrategy::Haircut,
        "poison" => TaintStrategy::Poison,
        "fifo" => TaintStrategy::Fifo,
        "lifo" => TaintStrategy::Lifo,
        other => return Err(ApiError::bad_request(format!("unknown strategy: {other}"))),
    };

    let result = state
        .risk()
        .trace(TraceRequest::new(
            TraceOrigin::Address(addr),
            direction,
            strategy,
            {
                let mut limits = TraceLimits::new(
                    q.max_hops.unwrap_or(10),
                    q.max_addresses.unwrap_or(1_000),
                    500,
                    Some(Ratio::from_percent(1)),
                );
                if let Some(s) = q.min_significance {
                    limits = limits.with_min_edge_significance(s);
                }
                limits
            },
            false,
        ))
        .await
        .map_err(ApiError::Internal)?;

    let sinks: Vec<SinkDto> = result
        .terminal_sinks()
        .iter()
        .map(|s| {
            let (kind, name) = sink_kind_str(s.kind());
            SinkDto {
                address: s.address().canonical(),
                kind: kind.into(),
                name,
                risk_score: s.risk_score(),
                tainted_amount: s.tainted_amount().raw().to_string(),
                formatted: format_amount(s.tainted_amount().raw(), s.tainted_amount().decimals()),
                taint_ratio: s.taint_ratio().as_f64(),
            }
        })
        .collect();

    let paths: Vec<PathDto> = result
        .paths()
        .iter()
        .map(|p| PathDto {
            depth: p.depth(),
            tainted_amount: p.tainted_amount().raw().to_string(),
            taint_ratio: p.taint_ratio().as_f64(),
            hops: p.hops().len(),
            origin: p.origin().map(|a| a.canonical()),
            destination: p.destination().map(|a| a.canonical()),
        })
        .collect();

    Ok(Json(TraceResponse {
        stats: TraceStatsDto {
            addresses_visited: result.stats().addresses_visited(),
            transfers_evaluated: result.stats().transfers_evaluated(),
            paths_found: result.stats().paths_found(),
            depth_reached: result.stats().depth_reached(),
            truncated: result.stats().truncated(),
        },
        sinks,
        paths,
    }))
}
