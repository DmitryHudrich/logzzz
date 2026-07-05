use std::sync::Arc;

use axum::{
    Router,
    middleware::{from_fn, from_fn_with_state},
    response::Redirect,
    routing::{get, post},
};
use clap::Parser;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use utoipa::OpenApi;
use utoipa_scalar::{Scalar, Servable};

use crate::config::{AppConfig, Cli};
use domain::chain::{ChainId, ChainRegistry};
use infra::{
    JobRepository, PostgresAddressKinds, PostgresAlerts, PostgresEntityRepository,
    PostgresTransferRepository, PostgresWatchlist, StaticLabelProvider, StaticPriceProvider,
};
use usecase::{AdaptiveConcurrency, IngestionService, RiskService};

mod bootstrap;
mod config;
mod error;
mod format;
mod handlers;
mod middleware;
mod openapi;
mod state;

use crate::bootstrap::{build_cors_layer, build_sources_from_chains, build_sources_legacy, init_tracer};
use crate::error::ApiError;
use crate::handlers::address_kind::{get_address_kind, set_address_kind};
use crate::handlers::batch::{sanctions_batch, score_batch};
use crate::handlers::chains::list_chains;
use crate::handlers::cluster::cluster_address;
use crate::handlers::edges::edge_significance_endpoint;
use crate::handlers::graph::get_graph;
use crate::handlers::heuristics::detect_heuristics;
use crate::handlers::jobs::{create_ingest_job, get_job_status};
use crate::handlers::labels::{
    LabelRequest, apply_label, labels_bulk, labels_delete, labels_get, labels_set,
};
use crate::handlers::path::shortest_path;
use crate::handlers::sanctions::check_sanctions;
use crate::handlers::score::score_address;
use crate::handlers::trace::trace_funds;
use crate::handlers::watchlist::{list_alerts, watchlist_add, watchlist_list, watchlist_remove};
use crate::middleware::{admin_auth_middleware, auth_middleware, version_middleware};
use crate::openapi::ApiDoc;
use crate::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = AppConfig::load(&Cli::parse())?;

    let provider = if cfg.telemetry().enabled() {
        Some(init_tracer(cfg.telemetry())?)
    } else {
        None
    };

    let otel_layer = provider.as_ref().map(|p| {
        use opentelemetry::trace::TracerProvider as _;
        tracing_opentelemetry::layer().with_tracer(p.tracer("api"))
    });

    tracing_subscriber::registry()
        .with(cfg.log().build_filter())
        .with(
            tracing_subscriber::fmt::layer()
                .compact()
                .with_writer(std::io::stderr),
        )
        .with(otel_layer)
        .init();

    if cfg.telemetry().enabled() {
        tracing::info!(
            endpoint = %cfg.telemetry().otlp_endpoint(),
            service = %cfg.telemetry().service_name(),
            "OTLP tracing enabled"
        );
    }

    let mut client_builder = reqwest::Client::builder();
    if let Some(url) = cfg.proxy().socks_url()
        && !url.is_empty()
    {
        tracing::info!(proxy = %url, "SOCKS proxy enabled");
        client_builder = client_builder.proxy(reqwest::Proxy::all(url)?);
    } else {
        tracing::info!("SOCKS proxy disabled");
    }
    let http_client = client_builder.build()?;

    let pool = infra::pg::create_pool(cfg.database().url())?;
    infra::pg::run_migrations(&pool).await?;

    let sources = if !cfg.chains().is_empty() {
        tracing::info!(
            chains = cfg.chains().len(),
            "wiring chain sources from chains: [...] config"
        );
        build_sources_from_chains(&cfg, &http_client).await?
    } else {
        tracing::warn!(
            "legacy config path in use: `ethereum:` + `trongrid.enabled` — \
             migrate to `chains: [...]` for multi-chain support"
        );
        build_sources_legacy(&cfg, &http_client).await?
    };

    if sources.is_empty() {
        anyhow::bail!(
            "no chain sources registered — populate `chains: [...]` or configure legacy `ethereum:` / `trongrid.enabled`"
        );
    }

    let chain_registry = ChainRegistry::default_registry();

    let api_key = cfg.server().api_key().map(str::to_owned);
    let admin_api_key = cfg.server().admin_api_key().map(str::to_owned);
    let entities_repo: Arc<PostgresEntityRepository> =
        Arc::new(PostgresEntityRepository::new(pool.clone()));

    let prices: Arc<StaticPriceProvider> = Arc::new(StaticPriceProvider::with_defaults());
    let labels: Arc<StaticLabelProvider> = Arc::new(StaticLabelProvider::new());
    let address_kinds: Arc<PostgresAddressKinds> =
        Arc::new(PostgresAddressKinds::new(pool.clone()));
    let watchlist: Arc<PostgresWatchlist> = Arc::new(PostgresWatchlist::new(pool.clone()));
    let alerts: Arc<PostgresAlerts> = Arc::new(PostgresAlerts::new(pool.clone()));

    let prices_for_state = Arc::clone(&prices);
    let watchlist_for_state = Arc::clone(&watchlist);
    let alerts_for_state = Arc::clone(&alerts);
    let kinds_for_state = Arc::clone(&address_kinds);
    let prices_for_ingest: Arc<dyn domain::ports::PricePort> = Arc::clone(&prices) as _;
    let watchlist_for_ingest: Arc<dyn domain::ports::WatchlistRepository> =
        Arc::clone(&watchlist) as _;
    let alerts_for_ingest: Arc<dyn domain::ports::AlertSink> = Arc::clone(&alerts) as _;
    let kinds_for_ingest: Arc<dyn domain::ports::AddressKindRepository> =
        Arc::clone(&address_kinds) as _;

    let transfers_concurrency = {
        let c = &cfg.ingestion().transfers_concurrency;
        Arc::new(AdaptiveConcurrency::new(
            c.initial,
            c.min,
            c.max,
            c.grow_after_successes,
        ))
    };
    tracing::info!(
        initial = cfg.ingestion().transfers_concurrency.initial,
        min = cfg.ingestion().transfers_concurrency.min,
        max = cfg.ingestion().transfers_concurrency.max,
        grow_after_successes = cfg.ingestion().transfers_concurrency.grow_after_successes,
        "ingestion transfers gate configured"
    );

    let state = Arc::new(AppState::new(
        IngestionService::new(
            sources,
            PostgresTransferRepository::new(pool.clone()),
            chain_registry.clone(),
        )
        .with_prices(prices_for_ingest)
        .with_watchlist(watchlist_for_ingest, alerts_for_ingest)
        .with_address_kinds(kinds_for_ingest)
        .with_transfers_concurrency(transfers_concurrency)
        .with_classify_chain_batch_size(cfg.ingestion().classify_chain_batch_size),
        RiskService::with_score_config(
            PostgresTransferRepository::new(pool.clone()),
            PostgresEntityRepository::new(pool.clone()),
            cfg.risk_cache().clone().into_domain(),
            cfg.heuristics().clone().into_domain(),
            cfg.score().clone().into_domain(),
        ),
        Arc::clone(&entities_repo),
        chain_registry,
        JobRepository::new(pool.clone()),
        api_key,
        admin_api_key,
        prices_for_state,
        labels,
        kinds_for_state,
        watchlist_for_state,
        alerts_for_state,
        ChainId::new(cfg.api().default_chain_id()),
    ));

    if let Some(path) = cfg.labels().bootstrap_file() {
        match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<Vec<LabelRequest>>(&text) {
                Ok(entries) => {
                    let mut applied = 0usize;
                    let mut failed = 0usize;
                    for req in entries {
                        match apply_label(&state, &req).await {
                            Ok(_) => applied += 1,
                            Err(e) => {
                                failed += 1;
                                let msg = match e {
                                    ApiError::BadRequest(s) => s,
                                    ApiError::Unauthorized => "unauthorized".into(),
                                    ApiError::Internal(de) => de.to_string(),
                                    ApiError::InternalMsg(s) => s,
                                };
                                tracing::warn!(path, address = %req.address, error = %msg, "bootstrap label failed");
                            }
                        }
                    }
                    tracing::info!(path, applied, failed, "bootstrap labels loaded");
                }
                Err(e) => {
                    tracing::warn!(path, error = %e, "bootstrap labels: invalid JSON");
                }
            },
            Err(e) => {
                tracing::warn!(path, error = %e, "bootstrap labels: read failed");
            }
        }
    }

    let addr = format!("{}:{}", cfg.server().host(), cfg.server().port());

    let admin_routes = Router::<Arc<AppState>>::new()
        .route("/labels", post(labels_set))
        .route("/labels/bulk", post(labels_bulk))
        .route("/labels/{addr}", get(labels_get).delete(labels_delete))
        .route("/watchlist", get(watchlist_list).post(watchlist_add).delete(watchlist_remove))
        .route("/alerts", get(list_alerts))
        .route("/address/{addr}/kind", post(set_address_kind))
        .layer(from_fn_with_state(
            Arc::clone(&state),
            admin_auth_middleware,
        ));

    let public_routes = Router::<Arc<AppState>>::new()
        .route("/chains", get(list_chains))
        .route("/graph", get(get_graph))
        .route("/score", get(score_address))
        .route("/score/batch", post(score_batch))
        .route("/sanctions", get(check_sanctions))
        .route("/sanctions/batch", post(sanctions_batch))
        .route("/trace", get(trace_funds))
        .route("/heuristics", get(detect_heuristics))
        .route("/path", get(shortest_path))
        .route("/cluster", get(cluster_address))
        .route("/edges/significance", get(edge_significance_endpoint))
        .route("/address/{addr}/kind", get(get_address_kind))
        .route("/jobs/ingest", post(create_ingest_job))
        .route("/jobs/{id}", get(get_job_status))
        .layer(from_fn_with_state(
            Arc::clone(&state),
            auth_middleware,
        ));

    let api = Router::<Arc<AppState>>::new()
        .merge(public_routes)
        .merge(admin_routes)
        .layer(from_fn(version_middleware));

    let app = Router::new()
        .merge(Scalar::with_url("/scalar", ApiDoc::openapi()))
        .route(
            "/swagger-ui",
            get(|| async { Redirect::permanent("/scalar") }),
        )
        .route(
            "/swagger-ui/",
            get(|| async { Redirect::permanent("/scalar") }),
        )
        .merge(api)
        .with_state(state)
        .layer(build_cors_layer(cfg.cors())?)
        .layer(TraceLayer::new_for_http().on_failure(()).on_response(
            |resp: &axum::response::Response,
             latency: std::time::Duration,
             _span: &tracing::Span| {
                if resp.status().is_server_error() {
                    tracing::error!(
                        status = resp.status().as_u16(),
                        latency_ms = latency.as_millis(),
                        "5xx"
                    );
                }
            },
        ));

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(addr, "listening");
    tokio::select! {
        result = axum::serve(listener, app) => { result?; }
        _ = shutdown_signal() => {}
    }

    if let Some(p) = provider {
        tracing::info!("flushing OTLP spans");
        let result = tokio::task::spawn_blocking(move || {
            if let Err(e) = p.force_flush() {
                eprintln!("WARN: force_flush failed: {e}");
            }
            if let Err(e) = p.shutdown() {
                eprintln!("ERROR: tracer provider shutdown failed: {e}");
            }
        })
        .await;
        if let Err(e) = result {
            tracing::error!(error = %e, "tracer shutdown task panicked");
        }
    }
    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigterm = signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = sigterm.recv() => {},
    }
    tracing::info!("shutdown signal received");
}
