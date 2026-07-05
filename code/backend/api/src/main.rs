use std::sync::Arc;
//
// #[global_allocator]
// static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Redirect},
    routing::{get, post},
};
use clap::Parser;
use serde::{Deserialize, Serialize};
use tower_http::trace::TraceLayer;
use tower_http::cors::{Any, CorsLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use utoipa::OpenApi;
use utoipa_scalar::{Scalar, Servable};

use crate::config::{AppConfig, Cli, EthSourceKind, TelemetryConfig};
use domain::chain::{ChainId, ChainRegistry};
use domain::entity::SanctionList;
use domain::error::DomainError;
use domain::graph::GraphRequest;
use domain::ports::{BlockRange, IngestionPort, RiskPort};
use domain::primitives::{Address, Ratio, U256};
use domain::risk::RiskSignalKind;
use domain::trace::{
    SinkKind, TaintStrategy, TraceDirection, TraceLimits, TraceOrigin, TraceRequest,
};
use domain::transfer::TransferKind;
use infra::fetch_wallet_api::MoralisEthSource;
use infra::{
    AlchemyEthSource, BigQueryEthSource, ChainSources, EtherscanEthSource, JobRepository,
    PostgresAddressKinds, PostgresAlerts, PostgresEntityRepository, PostgresTransferRepository,
    PostgresWatchlist, RoutedChains, RoutedEthSource, StaticLabelProvider, StaticPriceProvider,
    TronGridSource,
};
use usecase::{AdaptiveConcurrency, IngestionService, RiskService};

mod config;

pub struct AppState {
    ingestion: IngestionService<ChainSources, PostgresTransferRepository>,
    risk: RiskService<PostgresTransferRepository, PostgresEntityRepository>,
    entities: Arc<PostgresEntityRepository>,
    chains: ChainRegistry,
    jobs: JobRepository,
    api_key: Option<String>,
    admin_api_key: Option<String>,
    prices: Arc<StaticPriceProvider>,
    labels: Arc<StaticLabelProvider>,
    address_kinds: Arc<PostgresAddressKinds>,
    watchlist: Arc<PostgresWatchlist>,
    alerts: Arc<PostgresAlerts>,
}

impl AppState {
    pub fn new(
        ingestion: IngestionService<ChainSources, PostgresTransferRepository>,
        risk: RiskService<PostgresTransferRepository, PostgresEntityRepository>,
        entities: Arc<PostgresEntityRepository>,
        chains: ChainRegistry,
        jobs: JobRepository,
        api_key: Option<String>,
        admin_api_key: Option<String>,
        prices: Arc<StaticPriceProvider>,
        labels: Arc<StaticLabelProvider>,
        address_kinds: Arc<PostgresAddressKinds>,
        watchlist: Arc<PostgresWatchlist>,
        alerts: Arc<PostgresAlerts>,
    ) -> Self {
        Self {
            ingestion,
            risk,
            entities,
            chains,
            jobs,
            api_key,
            admin_api_key,
            prices,
            labels,
            address_kinds,
            watchlist,
            alerts,
        }
    }

    pub fn entities(&self) -> &PostgresEntityRepository {
        &self.entities
    }

    pub fn admin_api_key(&self) -> Option<&str> {
        self.admin_api_key.as_deref()
    }

    pub fn ingestion(&self) -> &IngestionService<ChainSources, PostgresTransferRepository> {
        &self.ingestion
    }

    pub fn risk(&self) -> &RiskService<PostgresTransferRepository, PostgresEntityRepository> {
        &self.risk
    }

    pub fn chains(&self) -> &ChainRegistry {
        &self.chains
    }

    pub fn jobs(&self) -> &JobRepository {
        &self.jobs
    }

    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    pub fn prices(&self) -> &StaticPriceProvider {
        &self.prices
    }

    pub fn labels(&self) -> &StaticLabelProvider {
        &self.labels
    }

    pub fn address_kinds(&self) -> &PostgresAddressKinds {
        &self.address_kinds
    }

    pub fn watchlist(&self) -> &PostgresWatchlist {
        &self.watchlist
    }

    pub fn alerts(&self) -> &PostgresAlerts {
        &self.alerts
    }
}

struct ApiSecurity;

impl utoipa::Modify for ApiSecurity {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityScheme};
        let components = openapi
            .components
            .get_or_insert_with(utoipa::openapi::Components::new);
        components.add_security_scheme(
            "api_version",
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::with_description(
                "X-API-Version",
                "Required for every request. Supported value: `1`.",
            ))),
        );
        components.add_security_scheme(
            "api_key",
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::with_description(
                "X-Api-Key",
                "Required only when server.api_key is configured. Bearer-token form \
                 (Authorization: Bearer <key>) is also accepted.",
            ))),
        );
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "PayTraces — Crypto Forensics API",
        version = "1.0.0",
        description = "PayTraces is a crypto-forensics API for EVM-compatible blockchains \
                       (Ethereum mainnet today; Tron and others coming). It builds a \
                       searchable graph of on-chain value transfers around an address, \
                       traces tainted funds to their final sinks, and produces a per-\
                       address risk score with explainable evidence.\n\n\
                       ---\n\n\
                       ## What you can do with this API\n\n\
                       - **Reconstruct the transfer graph** around any wallet — incoming \
                         and outgoing native + ERC-20 edges, paginated, with BFS depth \
                         and node-count caps you control.\n\
                       - **Trace tainted funds forward or backward** through multiple \
                         hops using FIFO, LIFO, Haircut, or Poison strategies — useful \
                         for AML investigations after a theft, hack, or sanctioned \
                         counterparty interaction.\n\
                       - **Score an address for risk** by aggregating signals from \
                         entity labels, sink exposure (mixer / sanctioned / darknet), \
                         and behavioural heuristics. Returns 0 (clean) to 100 (critical) \
                         with a list of contributing signals.\n\
                       - **Screen against sanctions lists** (OFAC / EU / UN) with a \
                         single GET. Bulk variant for batch checks.\n\
                       - **Detect cluster-formation patterns** — fan-in / fan-out / \
                         peeling chain / smurfing cycle / temporal burst / fixed-amount \
                         clustering / dwell time. Each detector produces evidence with \
                         the matching counterparties and a confidence band.\n\
                       - **Manage entity labels** (admin) — attach exchange / mixer / \
                         sanctioned / scam / bridge / darknet labels to addresses so \
                         downstream scoring and tracing benefit from your private \
                         attribution data.\n\n\
                       ---\n\n\
                       ## Authentication\n\n\
                       Two independent headers protect different parts of the API:\n\n\
                       | Header | When required | Endpoints |\n\
                       |--------|---------------|-----------|\n\
                       | `X-Api-Key` | Set on the server (optional) | All `/graph`, `/score`, `/sanctions`, `/trace`, `/heuristics`, ... |\n\
                       | `X-Admin-Api-Key` | Set on the server (optional) | Mutation endpoints: `POST /labels`, `POST /entities`, `POST /watchlist`, `POST /address/.../kind` |\n\
                       | `Authorization: Bearer <key>` | Alternative to `X-Api-Key` | Same as above |\n\n\
                       If the server has no API key configured, the corresponding headers \
                       are NOT required. The Scalar \"Authentication\" panel (top-right \
                       of this UI) lets you set both headers once per session.\n\n\
                       ---\n\n\
                       ## API versioning\n\n\
                       Every request MUST carry an `X-API-Version: 1` header. Without \
                       it the server returns `HTTP 400 missing required header`. With an \
                       unsupported value it returns the same status with the supported \
                       version listed. Only `/scalar` (this UI) and \
                       `/api-docs/openapi.json` (the raw spec) are exempt.\n\n\
                       This is a deliberately strict policy: it makes breaking schema \
                       changes safe to introduce on a new version while old clients \
                       keep working on `v1`.\n\n\
                       ---\n\n\
                       ## End-to-end workflow\n\n\
                       Most use cases follow the same shape: ingest first, then read.\n\n\
                       **Step 1.** Schedule ingestion for an address. This is async — \
                       it returns immediately with a job id while the worker walks the \
                       on-chain history and writes counterparty transfers to PostgreSQL.\n\
                       ```bash\n\
                       curl -X POST http://localhost:8080/jobs/ingest \\\n\
                         -H 'X-API-Version: 1' \\\n\
                         -H 'Content-Type: application/json' \\\n\
                         -d '{\n\
                           \"address\": \"0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045\",\n\
                           \"chain_id\": 1,\n\
                           \"max_depth\": 3,\n\
                           \"max_nodes\": 500\n\
                         }'\n\
                       # → { \"job_id\": \"01HX...\", \"status\": \"queued\" }\n\
                       ```\n\n\
                       **Step 2.** Poll job status until it succeeds.\n\
                       ```bash\n\
                       curl 'http://localhost:8080/jobs/01HX...' \\\n\
                         -H 'X-API-Version: 1'\n\
                       # → { \"status\": \"succeeded\", \"finished_at\": ... }\n\
                       ```\n\n\
                       **Step 3.** Read the transfer graph from the DB (this never \
                       touches a chain source — pure read).\n\
                       ```bash\n\
                       curl 'http://localhost:8080/graph?address=0xd8dA...&chain_id=1&max_depth=2&page=0&page_size=100' \\\n\
                         -H 'X-API-Version: 1'\n\
                       ```\n\n\
                       **Step 4.** Score and screen.\n\
                       ```bash\n\
                       curl 'http://localhost:8080/score?address=0xd8dA...&chain_id=1' \\\n\
                         -H 'X-API-Version: 1'\n\
                       # → { \"score\": 42, \"signals\": [...] }\n\
                       curl 'http://localhost:8080/sanctions?address=0xd8dA...&chain_id=1' \\\n\
                         -H 'X-API-Version: 1'\n\
                       ```\n\n\
                       **Step 5.** Inspect behavioural heuristics — fan-in, fan-out, \
                       smurfing, peeling, etc.\n\
                       ```bash\n\
                       curl 'http://localhost:8080/heuristics?address=0xd8dA...&chain_id=1' \\\n\
                         -H 'X-API-Version: 1'\n\
                       ```\n\n\
                       Or follow the money:\n\
                       ```bash\n\
                       curl 'http://localhost:8080/trace?address=0xd8dA...&chain_id=1&direction=forward&strategy=haircut&max_hops=5' \\\n\
                         -H 'X-API-Version: 1'\n\
                       ```\n\n\
                       ---\n\n\
                       ## Architecture\n\n\
                       - **Chain sources.** Per-chain, configurable. For Ethereum: \
                         Etherscan, Alchemy, Moralis, BigQuery, or a `routed` orchestrator \
                         that fails over between them on rate-limits. For Tron: TronGrid. \
                         Set via the `ethereum.source:` / `tron.source:` block in \
                         `config.yaml`.\n\
                       - **Storage.** PostgreSQL holds transfers, entity labels, address \
                         kinds (EOA / Contract / KnownService), watchlists, alerts. \
                         Read endpoints (`/graph`, `/score`, ...) hit only the DB, so \
                         they never block on a chain RPC.\n\
                       - **Ingestion.** `POST /jobs/ingest` enqueues a worker that walks \
                         the address graph BFS, fetches transfers from the configured \
                         chain source, persists them, and classifies counterparties as \
                         EOA vs. contract. Rate limits and retries are handled inside \
                         the source layer.\n\
                       - **Risk model.** `GET /score` aggregates RiskSignals (entity \
                         labels + sink exposure from forward/backward Haircut traces) \
                         using a configurable strategy (`max` or `weighted_count` with \
                         dedup). Tunables live under the `score:` block in `config.yaml`.\n\
                       - **Heuristics.** Cluster-formation detectors (fan-in/out, \
                         peeling, smurfing, burst, fixed-amount, dwell) feed into \
                         `GET /heuristics` and `POST /cluster`. Thresholds and windows \
                         live under the `heuristics:` block in `config.yaml`.\n\n\
                       ---\n\n\
                       ## Pagination\n\n\
                       `GET /graph` is paginated by edges. `nodes` is returned only on \
                       `page == 0` because the node set is global per request — paginate \
                       through edges using `page` + `page_size` (default 100, max 1000).\n\n\
                       Other list endpoints (`/labels`, `/watchlist`, `/alerts`) return \
                       the full collection in one response — these are admin endpoints \
                       expected to stay small.\n\n\
                       ---\n\n\
                       ## Common errors\n\n\
                       | Status | Meaning |\n\
                       |--------|---------|\n\
                       | `400 Bad Request` | Missing/invalid `X-API-Version`, malformed body, unknown chain id, address that doesn't parse for the chain family. |\n\
                       | `401 Unauthorized` | `X-Api-Key` or `X-Admin-Api-Key` missing or wrong. |\n\
                       | `404 Not Found` | Job id / entity id / label / watchlist entry not found. |\n\
                       | `409 Conflict` | Duplicate label or duplicate watchlist entry. |\n\
                       | `500 Internal Server Error` | Database, chain source, or internal bug. Response body carries the message. |\n\n\
                       All errors share the `ErrorResponse` schema documented below."
    ),
    tags(
        (name = "Graph",      description = "Build and read the transfer graph. \
                                            `POST /jobs/ingest` populates the DB \
                                            asynchronously; `GET /graph` reads it back \
                                            paginated."),
        (name = "Risk",       description = "Risk scoring, sanctions screening, fund \
                                            tracing, and behavioural heuristics. Read \
                                            from the DB only — run /jobs/ingest first \
                                            if data isn't there yet."),
        (name = "Labels",     description = "Admin-only entity / label CRUD. Use this to \
                                            attach OFAC, exchange, mixer, darknet, or \
                                            other attribution data to addresses; the \
                                            risk model and the trace sink classifier \
                                            consume those labels."),
        (name = "Watchlist",  description = "Admin-only watchlist of addresses. When a \
                                            saved ingestion touches a watched address, \
                                            an Alert is recorded automatically."),
        (name = "Alerts",     description = "Read-only stream of triggered watchlist \
                                            alerts (audit log)."),
        (name = "Jobs",       description = "Asynchronous ingestion jobs. Submit with \
                                            POST, poll with GET — the worker runs the \
                                            BFS and chain-source fetch off the request \
                                            path."),
        (name = "Chains",     description = "Supported blockchain registry (chain id, \
                                            family, address encoding, native asset)."),
        (name = "Discovery",  description = "Service discovery: which chains have a live \
                                            chain source registered, what each one can \
                                            do."),
    ),
    modifiers(&ApiSecurity),
    security(
        ("api_version" = []),
        ("api_key" = []),
    ),
    paths(
        get_graph, score_address, check_sanctions, trace_funds, list_chains,
        create_ingest_job, get_job_status,
        sanctions_batch, score_batch,
        detect_heuristics,
        shortest_path,
        cluster_address,
        watchlist_add, watchlist_list, watchlist_remove,
        list_alerts,
        get_address_kind, set_address_kind,
        edge_significance_endpoint,
        labels_set, labels_get, labels_delete, labels_bulk,
        entity_create, entity_get, entity_add_addresses, entity_remove_address,
    ),
    components(schemas(
        GraphPage, EdgeDto,
        ScoreResponse, SignalDto,
        SanctionsResponse,
        TraceResponse, TraceStatsDto, SinkDto, PathDto,
        ChainsResponse, ChainDto,
        ErrorResponse,
        IngestJobRequest, JobAcceptedResponse, JobStatusResponse,
        SanctionsBatchRequest, ScoreBatchRequest, BatchItem,
        HeuristicsResponse, HeuristicEvidenceDto,
        PathResponse, PathEdgeDto,
        ClusterResponse,
        WatchlistAddRequest, WatchlistEntryDto,
        AlertDto,
        AddressKindRequest, AddressKindResponse,
        EdgeSignificanceResponse, EdgeScoreDto,
        LabelRequest, LabelResponse, LabelsBulkResponse,
        EntityCreateRequest, EntityAddAddressesRequest,
    ))
)]
struct ApiDoc;

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

    let mut sources = ChainSources::builder();

    match cfg.ethereum().source() {
        EthSourceKind::Moralis => {
            if cfg.moralis().has_any_key() {
                let eth_source =
                    MoralisEthSource::new(http_client.clone(), cfg.moralis().clone().into_domain())
                        .await;
                sources = sources.register(eth_source);
                tracing::info!(chain = "eth", source = "moralis", "registered ETH source");
            } else {
                tracing::warn!("moralis: no api keys (api_key / api_keys empty) — Ethereum chain disabled");
            }
        }
        EthSourceKind::Bigquery => {
            let bq_cfg = cfg.bigquery().cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "ethereum.source=bigquery but `bigquery` section is missing in config"
                )
            })?;
            tracing::info!(
                project = %bq_cfg.project_id(),
                credentials_path = %bq_cfg.credentials_path(),
                "configuring BigQuery ETH source"
            );
            let eth_source =
                BigQueryEthSource::new(http_client.clone(), bq_cfg.into_domain()).await?;
            sources = sources.register(eth_source);
            tracing::info!(chain = "eth", source = "bigquery", "registered ETH source");
        }
        EthSourceKind::Etherscan => {
            let es_cfg = cfg.etherscan().cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "ethereum.source=etherscan but `etherscan` section is missing in config"
                )
            })?;
            if !es_cfg.has_any_key() {
                anyhow::bail!(
                    "ethereum.source=etherscan but no api keys configured — \
                     set etherscan.api_key, etherscan.api_keys, or pass --etherscan-api-key"
                );
            }
            let eth_source =
                EtherscanEthSource::new(http_client.clone(), es_cfg.into_domain()).await;
            sources = sources.register(eth_source);
            tracing::info!(chain = "eth", source = "etherscan", "registered ETH source");
        }
        EthSourceKind::Routed => {
            let routed_cfg = cfg.routed().cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "ethereum.source=routed but `routed` section is missing in config"
                )
            })?;
            let referenced = routed_cfg.referenced_sources();
            let mut builder = RoutedEthSource::builder(ChainId::ETH)
                .source_cooldown(routed_cfg.source_cooldown())
                .chains(RoutedChains {
                    transfers: routed_cfg.transfers.clone(),
                    is_contract: routed_cfg.is_contract.clone(),
                    latest_block: routed_cfg.latest_block.clone(),
                    fetch_block: routed_cfg.fetch_block.clone(),
                });

            // Construct each referenced leaf source on demand. The router's
            // own .build() will reject unknown names, but we want a sharper
            // error message when the config section itself is missing.
            for name in &referenced {
                match name.as_str() {
                    "etherscan" => {
                        let es_cfg = cfg.etherscan().cloned().ok_or_else(|| {
                            anyhow::anyhow!(
                                "routed chain references 'etherscan' but the `etherscan` section is missing"
                            )
                        })?;
                        if !es_cfg.has_any_key() {
                            anyhow::bail!(
                                "routed.* references 'etherscan' but etherscan has no api keys"
                            );
                        }
                        let src = EtherscanEthSource::new(
                            http_client.clone(),
                            es_cfg.into_domain(),
                        )
                        .await;
                        builder = builder.register("etherscan", src);
                    }
                    "alchemy" => {
                        let al_cfg = cfg.alchemy().cloned().ok_or_else(|| {
                            anyhow::anyhow!(
                                "routed chain references 'alchemy' but the `alchemy` section is missing"
                            )
                        })?;
                        if !al_cfg.has_any_key() {
                            anyhow::bail!(
                                "routed.* references 'alchemy' but alchemy has no api keys — \
                                 set alchemy.api_key / alchemy.api_keys or pass --alchemy-api-key"
                            );
                        }
                        let src =
                            AlchemyEthSource::new(http_client.clone(), al_cfg.into_domain());
                        builder = builder.register("alchemy", src);
                    }
                    "bigquery" => {
                        let bq_cfg = cfg.bigquery().cloned().ok_or_else(|| {
                            anyhow::anyhow!(
                                "routed chain references 'bigquery' but the `bigquery` section is missing"
                            )
                        })?;
                        let src = BigQueryEthSource::new(
                            http_client.clone(),
                            bq_cfg.into_domain(),
                        )
                        .await?;
                        builder = builder.register("bigquery", src);
                    }
                    "moralis" => {
                        if !cfg.moralis().has_any_key() {
                            anyhow::bail!(
                                "routed.* references 'moralis' but moralis has no api keys — \
                                 set moralis.api_key / moralis.api_keys or pass --moralis-api-key"
                            );
                        }
                        let src = MoralisEthSource::new(
                            http_client.clone(),
                            cfg.moralis().clone().into_domain(),
                        )
                        .await;
                        builder = builder.register("moralis", src);
                    }
                    other => anyhow::bail!(
                        "routed chain references unknown source '{other}' — \
                         supported: etherscan, alchemy, bigquery, moralis"
                    ),
                }
            }

            let router = builder.build()?;
            sources = sources.register(router);
            tracing::info!(
                chain = "eth",
                source = "routed",
                inner = ?referenced,
                "registered ETH source"
            );
        }
    }

    if cfg.trongrid().enabled() {
        let tron_source =
            TronGridSource::new(http_client.clone(), cfg.trongrid().clone().into_domain());
        sources = sources.register(tron_source);
        tracing::info!(chain = "tron", "registered TronGrid source");
    }

    let sources = sources.build();
    if sources.is_empty() {
        anyhow::bail!("no chain sources registered — check moralis/trongrid config");
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
        .route("/entities", post(entity_create))
        .route("/entities/{id}", get(entity_get))
        .route("/entities/{id}/addresses", post(entity_add_addresses))
        .route(
            "/entities/{id}/addresses/{addr}",
            axum::routing::delete(entity_remove_address),
        )
        .route("/watchlist", get(watchlist_list).post(watchlist_add).delete(watchlist_remove))
        .route("/alerts", get(list_alerts))
        .route("/address/{addr}/kind", post(set_address_kind))
        .layer(middleware::from_fn_with_state(
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
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth_middleware,
        ));

    let api = Router::<Arc<AppState>>::new()
        .merge(public_routes)
        .merge(admin_routes)
        .layer(middleware::from_fn(version_middleware));

    // Scalar serves the interactive API docs at /scalar; the raw spec
    // stays at /api-docs/openapi.json so existing tooling (codegen, lint,
    // import-into-Postman) keeps working without a path change.
    //
    // /swagger-ui is kept as a permanent redirect so any old links or
    // bookmarks from the previous swagger-ui-based UI land on Scalar
    // instead of 404. Mount on the outer router so the version_middleware
    // (which only wraps the `api` sub-router) does not gate the redirect.
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
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
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

fn init_tracer(
    cfg: &TelemetryConfig,
) -> anyhow::Result<opentelemetry_sdk::trace::SdkTracerProvider> {
    use opentelemetry::KeyValue;
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::Resource;
    use opentelemetry_sdk::trace::{BatchConfigBuilder, BatchSpanProcessor, SdkTracerProvider};

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(cfg.otlp_endpoint())
        .build()?;

    let resource = Resource::builder_empty()
        .with_attribute(KeyValue::new(
            "service.name",
            cfg.service_name().to_string(),
        ))
        .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
        .build();

    let processor = BatchSpanProcessor::builder(exporter)
        .with_batch_config(
            BatchConfigBuilder::default()
                .with_scheduled_delay(std::time::Duration::from_secs(1))
                .build(),
        )
        .build();

    let provider = SdkTracerProvider::builder()
        .with_span_processor(processor)
        .with_resource(resource)
        .build();

    opentelemetry::global::set_tracer_provider(provider.clone());
    Ok(provider)
}

fn parse_address(s: &str, chain: ChainId) -> Result<Address, ApiError> {
    Address::parse(chain, s).map_err(|e| ApiError::bad_request(e.to_string()))
}

fn format_amount(raw: U256, decimals: u8) -> String {
    let s = raw.to_string();
    if decimals == 0 {
        return s;
    }
    let dec = decimals as usize;
    let (int_part, frac_part) = if s.len() <= dec {
        (String::from("0"), format!("{:0>w$}", s, w = dec))
    } else {
        let split = s.len() - dec;
        (s[..split].to_string(), s[split..].to_string())
    };
    let frac_trunc = if frac_part.len() > 8 {
        &frac_part[..8]
    } else {
        &frac_part[..]
    };
    let frac_trim = frac_trunc.trim_end_matches('0');
    if frac_trim.is_empty() {
        int_part
    } else {
        format!("{int_part}.{frac_trim}")
    }
}

fn native_symbol(chains: &ChainRegistry, chain: ChainId) -> String {
    chains
        .get(chain)
        .map(|m| m.native_asset_symbol().to_string())
        .unwrap_or_default()
}

fn transfer_kind_str(k: &TransferKind) -> (&'static str, Option<String>) {
    match k {
        TransferKind::Native => ("native", None),
        TransferKind::Token { contract, .. } => ("token", Some(contract.canonical())),
        TransferKind::Internal => ("internal", None),
        TransferKind::Fee => ("fee", None),
        TransferKind::UtxoEdge { .. } => ("utxo_edge", None),
    }
}

fn edge_symbol(k: &TransferKind, native: &str) -> String {
    match k {
        TransferKind::Native | TransferKind::Internal | TransferKind::Fee => native.to_string(),
        TransferKind::Token { symbol, .. } => symbol.clone().unwrap_or_default(),
        TransferKind::UtxoEdge { .. } => native.to_string(),
    }
}

fn sink_kind_str(k: &SinkKind) -> (&'static str, Option<String>) {
    match k {
        SinkKind::Exchange { name, .. } => ("exchange", Some(name.clone())),
        SinkKind::Bridge { .. } => ("bridge", None),
        SinkKind::Mixer => ("mixer", None),
        SinkKind::Sanctioned => ("sanctioned", None),
        SinkKind::Darknet => ("darknet", None),
        SinkKind::Unresolved => ("unresolved", None),
    }
}

fn signal_kind_str(k: &RiskSignalKind) -> &'static str {
    match k {
        RiskSignalKind::DirectExposure => "direct_exposure",
        RiskSignalKind::IndirectExposure { .. } => "indirect_exposure",
        RiskSignalKind::SanctionedCounterparty => "sanctioned_counterparty",
        RiskSignalKind::MixerInteraction => "mixer_interaction",
        RiskSignalKind::DarknetMarket => "darknet_market",
        RiskSignalKind::RapidLayering => "rapid_layering",
        RiskSignalKind::HighVelocity => "high_velocity",
        RiskSignalKind::NewAddress => "new_address",
        RiskSignalKind::NoKyc => "no_kyc",
    }
}

fn sanction_list_str(s: &SanctionList) -> String {
    match s {
        SanctionList::Ofac => "ofac".into(),
        SanctionList::Eu => "eu".into(),
        SanctionList::Un => "un".into(),
        SanctionList::Other(s) => s.to_lowercase().replace([' ', '-'], "_"),
    }
}

// ── Response DTOs ────────────────────────────────────────────────────────────

/// A single transfer edge in the on-chain graph returned by `GET /graph`.
///
/// One `EdgeDto` corresponds to exactly one value transfer (a native ETH/TRX
/// move, an ERC-20/TRC-20 token transfer, an internal contract call, a gas fee,
/// or a UTXO edge for Bitcoin-family chains). `from` always lost the value and
/// `to` always received it.
#[derive(Serialize, utoipa::ToSchema)]
pub struct EdgeDto {
    /// Transaction hash, hex-encoded without the `0x` prefix (64 chars on EVM).
    #[schema(example = "a1b2c3...")]
    tx_hash: String,
    /// Log / event index within the transaction. `0` for a native top-level
    /// transfer; higher values disambiguate multiple token transfers inside the
    /// same tx.
    index: u32,
    /// Canonical sender address (the one that lost the value).
    from: String,
    /// Canonical recipient address (the one that gained the value).
    to: String,
    /// Raw on-chain amount as a base-10 big integer string, with no decimal
    /// shift applied. For ETH this is wei; for an ERC-20 it is in the token's
    /// own base units.
    #[schema(example = "1000000000000000000")]
    raw: String,
    /// Human-friendly amount: `raw / 10^decimals`, truncated to at most 8
    /// fractional digits with trailing zeros stripped.
    #[schema(example = "1.0")]
    formatted: String,
    /// Asset symbol for the edge — token symbol for ERC-20 / TRC-20 transfers,
    /// otherwise the chain's native asset symbol (`ETH`, `TRX`, ...). May be
    /// empty for tokens whose contract did not expose a symbol.
    #[schema(example = "ETH")]
    symbol: String,
    /// Token decimals used by `formatted` (18 for ETH and most ERC-20 tokens,
    /// 6 for USDC/USDT).
    decimals: u8,
    /// Block height at which the transfer was mined.
    block: u64,
    /// Unix timestamp of the block in seconds (UTC).
    ts: i64,
    /// Transfer kind. One of: `native` (chain's native asset), `token`
    /// (ERC-20 / TRC-20), `internal` (internal contract-call value move),
    /// `fee` (gas / transaction fee), `utxo_edge` (Bitcoin-family edge).
    kind: String,
    /// Token contract address when `kind == "token"`. `null` for every other
    /// kind.
    contract: Option<String>,
    /// Numeric chain id this edge belongs to (echoed from the request).
    chain_id: u32,
}

/// One page of a paginated transfer graph, returned by `GET /graph`.
///
/// Pagination is over `edges` only — the `nodes` array is global to the request
/// and only ships on page 0 to avoid duplicating it across pages. Pages are
/// returned in stable repository order; missing data means ingestion has not
/// run yet (see `POST /jobs/ingest`).
#[derive(Serialize, utoipa::ToSchema)]
pub struct GraphPage {
    /// Total number of unique addresses (nodes) in the full graph, across all
    /// pages.
    total_nodes: usize,
    /// Total number of transfers (edges) in the full graph, across all pages.
    total_edges: usize,
    /// Current page index, zero-based. Always equals the requested `page`
    /// parameter (or 0 when omitted).
    page: u32,
    /// Number of edges per page, after clamping to the server's bounds
    /// (default 100, max 1000).
    page_size: usize,
    /// Total number of pages available for this request.
    total_pages: u32,
    /// Convenience flag: `true` when a strictly higher `page` value still
    /// returns edges.
    has_next: bool,
    /// All unique addresses in the graph, in deterministic order. Populated
    /// only on `page == 0`; on subsequent pages this array is empty to keep
    /// each page payload bounded.
    nodes: Vec<String>,
    /// Transfers on the current page.
    edges: Vec<EdgeDto>,
}

/// One contributing risk signal inside a [`ScoreResponse`].
///
/// Each signal is a single, explainable reason why the address received the
/// score it did — for example direct exposure to a sanctioned counterparty,
/// or a mixer interaction. Severities combine into the aggregate `score`.
#[derive(Serialize, utoipa::ToSchema)]
pub struct SignalDto {
    /// Stable snake_case category. One of: `direct_exposure`,
    /// `indirect_exposure`, `sanctioned_counterparty`, `mixer_interaction`,
    /// `darknet_market`, `rapid_layering`, `high_velocity`, `new_address`,
    /// `no_kyc`.
    kind: String,
    /// Severity in the range 0–100. `>= 70` is considered HIGH, `>= 90` is
    /// CRITICAL. The overall `score` is derived from these severities using
    /// the configured aggregation strategy.
    severity: u8,
    /// Human-readable explanation of what triggered this signal — useful to
    /// surface verbatim in an investigator UI.
    description: String,
}

/// Risk-score report for a single address, returned by `GET /score` and as
/// each item of `POST /score/batch`.
///
/// The overall `score` is the aggregation of every signal in `signals` using
/// the `score.aggregation_strategy` configured server-side (`max` or
/// `weighted_count`). Signals come from entity labels (`/labels`) and from
/// taint-trace sink classification — run `POST /jobs/ingest` first if the
/// address has no persisted history.
#[derive(Serialize, utoipa::ToSchema)]
pub struct ScoreResponse {
    /// Canonical form of the scored address (echoes the request).
    address: String,
    /// Numeric chain id this score applies to.
    chain_id: u32,
    /// Aggregate risk score in the range 0–100. Higher means riskier.
    score: u8,
    /// Convenience flag: `true` when `score >= 70`.
    is_high_risk: bool,
    /// Every individual signal that contributed to the score. May be empty when
    /// the address is clean (in which case `score` is 0 and `is_high_risk` is
    /// `false`).
    signals: Vec<SignalDto>,
    /// ISO-8601 timestamp (UTC) when the report was computed. Useful for cache
    /// invalidation in a client.
    generated_at: String,
}

/// Sanctions-screening result returned by `GET /sanctions` and each item of
/// `POST /sanctions/batch`.
///
/// Combines OFAC / EU / UN list checks with the server's internal entity
/// labels. A `true` here covers both direct sanctions hits and sanctioned-by-
/// inheritance cases (e.g. an address attached to a sanctioned entity).
#[derive(Serialize, utoipa::ToSchema)]
pub struct SanctionsResponse {
    /// Canonical form of the screened address.
    address: String,
    /// Numeric chain id.
    chain_id: u32,
    /// `true` if the address appears on any configured sanctions list.
    is_sanctioned: bool,
    /// Which list flagged the address. One of `ofac`, `eu`, `un`, or a custom
    /// snake_case identifier for non-standard lists. `null` when
    /// `is_sanctioned` is `false`.
    sanction_list: Option<String>,
    /// Best-known entity label for the address (e.g. `"Tornado Cash"`,
    /// `"Lazarus Group"`). `null` when no label is attached.
    label: Option<String>,
}

/// Aggregate statistics describing how much work a trace did.
///
/// Returned inside [`TraceResponse`]. Useful for diagnosing under-/over-budget
/// traces: when `truncated` is `true`, the trace stopped because it hit a
/// limit, not because it ran out of paths.
#[derive(Serialize, utoipa::ToSchema)]
pub struct TraceStatsDto {
    /// Number of unique addresses the trace visited during traversal.
    addresses_visited: usize,
    /// Total number of transfer edges the trace evaluated. Higher than
    /// `addresses_visited` because each address has multiple in/out edges.
    transfers_evaluated: usize,
    /// Number of complete origin-to-sink paths discovered.
    paths_found: usize,
    /// Deepest hop reached by the BFS. Always `<= max_hops` from the request.
    depth_reached: u32,
    /// `true` when the trace hit `max_hops`, `max_addresses`, or the internal
    /// `max_paths` cap. When `true`, the result is a lower bound on what's
    /// actually reachable.
    truncated: bool,
}

/// A terminal "sink" where tainted funds came to rest during a trace.
///
/// Returned inside [`TraceResponse::sinks`]. A sink is the last address on a
/// traced path where the taint stops moving (because the funds were cashed
/// out, mixed, bridged, or simply held). The `kind` and `risk_score` come from
/// entity labels and built-in classifiers.
#[derive(Serialize, utoipa::ToSchema)]
pub struct SinkDto {
    /// Canonical address of the sink.
    address: String,
    /// Sink category. One of: `exchange`, `bridge`, `mixer`, `sanctioned`,
    /// `darknet`, `unresolved`. `unresolved` means the sink could not be
    /// classified from labels.
    kind: String,
    /// Human name for the sink when `kind == "exchange"` (e.g. `"Binance"`).
    /// `null` for every other kind.
    name: Option<String>,
    /// Risk score 0–100 attached to the sink itself. Higher means
    /// investigating this sink should be prioritised.
    risk_score: u8,
    /// Raw tainted amount that reached this sink, as a base-10 big integer
    /// string in the asset's base units (wei for ETH).
    tainted_amount: String,
    /// Decimal-shifted tainted amount, truncated to at most 8 fractional
    /// digits with trailing zeros stripped.
    formatted: String,
    /// Fraction of the sink's incoming funds that came from the tainted
    /// origin (0.0–1.0). `1.0` means the sink received only tainted funds on
    /// the observed paths.
    taint_ratio: f64,
}

/// One end-to-end traced path from an origin address to a sink.
///
/// Returned inside [`TraceResponse::paths`]. Each path is a chain of hops; this
/// DTO captures the path's shape (depth, hop count) and its taint summary
/// rather than every individual edge.
#[derive(Serialize, utoipa::ToSchema)]
pub struct PathDto {
    /// Path depth (== `hops`). Number of edges along the path.
    depth: u32,
    /// Raw tainted amount carried along this specific path, as a base-10 big
    /// integer string.
    tainted_amount: String,
    /// Taint ratio at the destination of the path (0.0–1.0).
    taint_ratio: f64,
    /// Number of transfers (hops) in the path.
    hops: usize,
    /// Canonical address where the path starts (the traced subject for forward
    /// traces, or the source for backward traces). `null` for synthetic paths
    /// that have no concrete starting address.
    origin: Option<String>,
    /// Canonical address where the path ends (a sink). `null` for paths that
    /// were truncated before reaching a sink.
    destination: Option<String>,
}

/// Result of a fund-flow trace, returned by `GET /trace`.
///
/// Combines aggregate statistics, the set of terminal sinks reached by the
/// taint, and a list of the actual paths discovered. The same sink may be the
/// endpoint of several paths.
#[derive(Serialize, utoipa::ToSchema)]
pub struct TraceResponse {
    /// How much work the trace did and whether it was truncated.
    stats: TraceStatsDto,
    /// Terminal sinks reached by tainted funds, deduplicated by address.
    sinks: Vec<SinkDto>,
    /// Origin-to-sink paths in the order they were discovered.
    paths: Vec<PathDto>,
}

/// Metadata for a single chain known to the server's chain registry.
///
/// Returned as part of [`ChainsResponse`] by `GET /chains`. `source_registered`
/// tells you whether the server has a live data source configured for this chain
/// (without one, ingestion and live reads will fail for that chain even though
/// the chain itself is recognised by the registry).
#[derive(Serialize, utoipa::ToSchema)]
pub struct ChainDto {
    /// Numeric chain id (e.g. `1` for Ethereum mainnet, `728126428` for Tron mainnet).
    id: u32,
    /// Human-readable chain name (e.g. `"Ethereum"`, `"Tron"`).
    name: String,
    /// Chain family. One of: `evm`, `tron`, `bitcoin`, `solana`, `other`.
    family: String,
    /// Address ledger model. One of: `account` (account-balance model, EVM/Tron/Solana)
    /// or `utxo` (Bitcoin-style unspent outputs).
    address_model: String,
    /// On-the-wire address encoding. One of: `hex20` (EVM 20-byte hex),
    /// `tron_base58_check`, `bech32`, `base58`.
    address_encoding: String,
    /// Symbol of the chain's native asset (e.g. `"ETH"`, `"TRX"`).
    native_symbol: String,
    /// Number of decimal places for the native asset (18 for ETH, 6 for TRX).
    native_decimals: u8,
    /// Number of blocks the chain considers "final" for confirmation purposes.
    /// Used by ingestion to wait out reorgs.
    confirmation_depth: u64,
    /// `true` when the server has a live chain source registered for this chain
    /// and ingestion/reads are operational. `false` means the chain is in the
    /// registry but no source is configured — ingestion will fail.
    source_registered: bool,
}

/// Envelope returned by `GET /chains`. Lists every chain the server knows about.
#[derive(Serialize, utoipa::ToSchema)]
pub struct ChainsResponse {
    /// All chains in the registry, in registry order. Includes both chains
    /// with a live source (`source_registered == true`) and chains that are
    /// recognised but not currently operational.
    chains: Vec<ChainDto>,
}

/// Error response body.
#[derive(Serialize, utoipa::ToSchema)]
pub struct ErrorResponse {
    error: String,
}

// ── API error type ───────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ErrBody {
    error: String,
}

pub enum ApiError {
    BadRequest(String),
    Unauthorized,
    Internal(DomainError),
    InternalMsg(String),
}

impl ApiError {
    fn bad_request(msg: impl Into<String>) -> Self {
        Self::BadRequest(msg.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        match self {
            Self::BadRequest(msg) => {
                (StatusCode::BAD_REQUEST, Json(ErrBody { error: msg })).into_response()
            }
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                Json(ErrBody {
                    error: "unauthorized".into(),
                }),
            )
                .into_response(),
            Self::Internal(e) => {
                tracing::error!(error = %e, "domain error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrBody {
                        error: "internal server error".into(),
                    }),
                )
                    .into_response()
            }
            Self::InternalMsg(msg) => {
                tracing::error!(error = %msg, "internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrBody {
                        error: "internal server error".into(),
                    }),
                )
                    .into_response()
            }
        }
    }
}

// ── Versioning middleware ────────────────────────────────────────────────────

const API_VERSION_HEADER: &str = "X-API-Version";
const SUPPORTED_API_VERSION: &str = "1";

async fn version_middleware(
    req: axum::extract::Request,
    next: Next,
) -> Result<axum::response::Response, ApiError> {
    match req
        .headers()
        .get(API_VERSION_HEADER)
        .and_then(|v| v.to_str().ok())
    {
        Some(v) if v == SUPPORTED_API_VERSION => Ok(next.run(req).await),
        Some(other) => Err(ApiError::bad_request(format!(
            "unsupported {API_VERSION_HEADER}: {other}; supported: {SUPPORTED_API_VERSION}"
        ))),
        None => Err(ApiError::bad_request(format!(
            "missing required header: {API_VERSION_HEADER}"
        ))),
    }
}

// ── Auth middleware ──────────────────────────────────────────────────────────

async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    req: axum::extract::Request,
    next: Next,
) -> Result<axum::response::Response, ApiError> {
    let Some(expected) = state.api_key() else {
        return Ok(next.run(req).await);
    };

    let headers = req.headers();
    let provided = headers
        .get("X-Api-Key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or_else(|| {
            headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.strip_prefix("Bearer "))
                .map(str::to_string)
        });

    match provided {
        Some(p) if p == expected => Ok(next.run(req).await),
        _ => Err(ApiError::Unauthorized),
    }
}

async fn admin_auth_middleware(
    State(state): State<Arc<AppState>>,
    req: axum::extract::Request,
    next: Next,
) -> Result<axum::response::Response, ApiError> {
    let Some(expected) = state.admin_api_key() else {
        // If no admin key is configured, fall back to plain api_key behaviour
        // — admin endpoints stay protected by the regular key.
        return auth_middleware(State(state), req, next).await;
    };

    let provided = req
        .headers()
        .get("X-Admin-Api-Key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    match provided {
        Some(p) if p == expected => Ok(next.run(req).await),
        _ => Err(ApiError::Unauthorized),
    }
}

// ── Handlers ─────────────────────────────────────────────────────────────────

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
        (status = 200,
         description = "Full list of registered chains. Each entry carries the numeric \
                        chain id, family, native asset metadata, confirmation depth, and a \
                        `source_registered` flag distinguishing live chains from \
                        registry-only entries.",
         body = ChainsResponse),
    ),
    tag = "Discovery"
)]
pub async fn list_chains(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    use domain::chain::{AddressEncoding, AddressModel, ChainFamily};
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
            address_encoding: match m.address_encoding() {
                AddressEncoding::Hex20 => "hex20",
                AddressEncoding::TronBase58Check => "tron_base58_check",
                AddressEncoding::Bech32 => "bech32",
                AddressEncoding::Base58 => "base58",
            }
            .into(),
            native_symbol: m.native_asset_symbol().to_string(),
            native_decimals: m.native_asset_decimals(),
            confirmation_depth: m.confirmation_depth(),
            source_registered: registered.contains(&m.id()),
        })
        .collect();
    Json(ChainsResponse { chains })
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct GraphQuery {
    /// Origin wallet address
    #[param(example = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")]
    address: String,

    /// Chain ID. Defaults to 1 (Ethereum mainnet).
    #[param(example = 1)]
    chain_id: Option<u32>,

    /// How many hops to traverse from the origin. Defaults to 3.
    #[param(example = 2, minimum = 1)]
    max_depth: Option<u32>,

    /// Hard cap on the total number of nodes in the graph. Defaults to 500.
    #[param(example = 500)]
    max_nodes: Option<usize>,

    /// Per-address cap on transfers fetched from the chain/repo. Defaults to 10000.
    #[param(example = 10000)]
    max_transfers_per_address: Option<usize>,

    /// Restrict transfers to blocks ≥ this height (inclusive).
    #[param(example = 19000000)]
    from_block: Option<u64>,

    /// Restrict transfers to blocks ≤ this height (inclusive).
    #[param(example = 20000000)]
    to_block: Option<u64>,

    /// Page index (0-based). Defaults to 0. Nodes are returned only on `page == 0`.
    #[param(example = 0)]
    page: Option<u32>,

    /// Number of edges per page. Defaults to 100, maximum 1000.
    #[param(example = 100)]
    page_size: Option<usize>,
}

#[utoipa::path(
    get, path = "/graph",
    description = "Read the persisted transfer graph around an address, paginated by edge.\n\n\
                   ## What this does\n\n\
                   Walks the persisted graph in PostgreSQL outward from `address`, BFS-bounded \
                   by `max_depth` hops and `max_nodes` distinct counterparties, then returns the \
                   discovered transfers paginated by edge. This endpoint never contacts a chain \
                   RPC; it is a pure read against whatever ingestion has already persisted, so \
                   the response is fast and deterministic but bounded by what has already been \
                   ingested.\n\n\
                   ## When to use it\n\n\
                   Run `POST /jobs/ingest` for the same address first and wait for the job to \
                   reach `succeeded`. Then call this endpoint to render the graph, drive a \
                   visualisation, or feed it into downstream tooling. If you call it before \
                   ingestion, you will get an empty graph rather than an error.\n\n\
                   ## Example\n\n\
                   ```bash\n\
                   curl 'http://localhost:8080/graph?address=0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045&chain_id=1&max_depth=2&page=0&page_size=100' \\\n\
                     -H 'X-API-Version: 1'\n\
                   ```\n\n\
                   ## Notes\n\n\
                   The `nodes` array is only returned on `page == 0` because the node set is \
                   global per request; subsequent pages send an empty `nodes` array to save \
                   bandwidth. `page_size` is clamped to `[1, 1000]`. The `from_block` / \
                   `to_block` filters apply to the persisted edges only.",
    params(GraphQuery),
    responses(
        (status = 200,
         description = "One page of the persisted graph. `total_nodes` / `total_edges` describe \
                        the full result set; `edges` carries the current page only; `nodes` is \
                        non-empty only on page 0. An empty graph (no nodes, no edges) means \
                        ingestion has not produced data for this address yet — run \
                        `POST /jobs/ingest`.",
         body = GraphPage),
        (status = 400,
         description = "Address could not be parsed for the chain family, an unknown `chain_id` \
                        was supplied, or numeric parameters (page, page_size, max_depth, etc.) \
                        were out of range.",
         body = ErrorResponse),
        (status = 500,
         description = "Database read failure. The original error message is included in the \
                        body for debugging.",
         body = ErrorResponse),
    ),
    tag = "Graph"
)]
pub async fn get_graph(
    State(state): State<Arc<AppState>>,
    Query(q): Query<GraphQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let chain = ChainId::new(q.chain_id.unwrap_or(ChainId::ETH.value()));
    let addr = parse_address(&q.address, chain)?;

    let range = match (q.from_block, q.to_block) {
        (None, None) => None,
        (from, to) => Some(BlockRange::new(from.unwrap_or(0), to.unwrap_or(u64::MAX))),
    };

    let graph = state
        .ingestion()
        .build_graph_from_db(
            &addr,
            GraphRequest::new(
                range,
                q.max_depth.unwrap_or(3),
                q.max_nodes.unwrap_or(500),
                q.max_transfers_per_address.unwrap_or(10_000),
            ),
        )
        .await
        .map_err(ApiError::Internal)?;

    let page = q.page.unwrap_or(0) as usize;
    let page_size = q.page_size.unwrap_or(100).clamp(1, 1000);

    let total_nodes = graph.nodes().len();
    let total_edges = graph.edges().len();
    let total_pages = total_edges.div_ceil(page_size).max(1) as u32;

    let start = page * page_size;
    let edge_slice = graph.edges().get(start..).unwrap_or(&[]);
    let edge_page = &edge_slice[..edge_slice.len().min(page_size)];

    let nodes: Vec<String> = if page == 0 {
        graph.nodes().iter().map(|a| a.canonical()).collect()
    } else {
        Vec::new()
    };

    let native = native_symbol(state.chains(), chain);

    let edges: Vec<EdgeDto> = edge_page
        .iter()
        .map(|t| {
            let (kind, contract) = transfer_kind_str(t.kind());
            EdgeDto {
                tx_hash: hex::encode(t.tx_ref().hash()),
                index: t.id().index(),
                from: t.from().canonical(),
                to: t.to().canonical(),
                raw: t.amount().raw().to_string(),
                formatted: format_amount(t.amount().raw(), t.amount().decimals()),
                symbol: edge_symbol(t.kind(), &native),
                decimals: t.amount().decimals(),
                block: t.block().height(),
                ts: t.timestamp().timestamp(),
                kind: kind.into(),
                contract,
                chain_id: t.chain().value(),
            }
        })
        .collect();

    Ok(Json(GraphPage {
        total_nodes,
        total_edges,
        page: page as u32,
        page_size,
        total_pages,
        has_next: page as u32 + 1 < total_pages,
        nodes,
        edges,
    }))
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ScoreQuery {
    #[param(example = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")]
    address: String,
    #[param(example = 1)]
    chain_id: Option<u32>,
}

#[utoipa::path(
    get, path = "/score",
    description = "Compute a 0–100 risk score for an address with explainable signals.\n\n\
                   ## What this does\n\n\
                   Aggregates risk signals attached to the address into a single 0–100 score. \
                   Inputs are entity labels (from `/labels`), built-in sanctions lists, and the \
                   sinks discovered by an internal forward + backward Haircut trace over the \
                   persisted graph. Signals are combined using the strategy configured under \
                   the `score:` block in `config.yaml` (`max` by default, or `weighted_count` \
                   with deduplication).\n\n\
                   This is a DB-only read — the chain RPC is never touched. If you score an \
                   address that has not been ingested, you will get a low score even when the \
                   real on-chain reality says otherwise: ingest first.\n\n\
                   ## When to use it\n\n\
                   After running `POST /jobs/ingest` for the address, call this whenever you \
                   need a single risk number plus explainable evidence. Surface the `signals` \
                   array verbatim in your UI so investigators can audit each contributor.\n\n\
                   ## Example\n\n\
                   ```bash\n\
                   curl 'http://localhost:8080/score?address=0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045&chain_id=1' \\\n\
                     -H 'X-API-Version: 1'\n\
                   ```\n\n\
                   ## Notes\n\n\
                   The score is cached for a short, configurable TTL (`risk_cache:` block in \
                   `config.yaml`), so back-to-back calls return the same result without redoing \
                   the trace.",
    params(ScoreQuery),
    responses(
        (status = 200,
         description = "Risk report containing the aggregate score and every signal that \
                        contributed to it. An empty `signals` array with `score: 0` means the \
                        address looks clean to the model — verify ingestion actually covered \
                        it before drawing conclusions.",
         body = ScoreResponse),
        (status = 400,
         description = "Address could not be parsed for the chain family, or `chain_id` is \
                        unknown.",
         body = ErrorResponse),
        (status = 500,
         description = "Database or internal scoring failure. The error message is included \
                        in the body.",
         body = ErrorResponse),
    ),
    tag = "Risk"
)]
pub async fn score_address(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ScoreQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let chain = ChainId::new(q.chain_id.unwrap_or(ChainId::ETH.value()));
    let addr = parse_address(&q.address, chain)?;

    let report = state
        .risk()
        .score(&addr)
        .await
        .map_err(ApiError::Internal)?;

    Ok(Json(report_to_dto(&report)))
}

fn report_to_dto(report: &domain::risk::RiskReport) -> ScoreResponse {
    let signals: Vec<SignalDto> = report
        .signals()
        .iter()
        .map(|s| SignalDto {
            kind: signal_kind_str(s.kind()).into(),
            severity: s.severity().value(),
            description: s.description().to_string(),
        })
        .collect();

    ScoreResponse {
        address: report.subject().canonical(),
        chain_id: report.subject().chain().value(),
        score: report.overall_score().value(),
        is_high_risk: report.is_high_risk(),
        signals,
        generated_at: report.generated_at().to_rfc3339(),
    }
}

fn sanctions_to_dto(result: &domain::risk::SanctionsCheckResult) -> SanctionsResponse {
    SanctionsResponse {
        address: result.address().canonical(),
        chain_id: result.address().chain().value(),
        is_sanctioned: result.is_sanctioned(),
        sanction_list: result.sanction_list().map(sanction_list_str),
        label: result.label().map(str::to_string),
    }
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
        (status = 200,
         description = "Sanctions verdict. `is_sanctioned` is `true` when the address is on a \
                        list; `sanction_list` and `label` are populated only on a hit.",
         body = SanctionsResponse),
        (status = 400,
         description = "Address failed to parse for the chain family, or `chain_id` is unknown.",
         body = ErrorResponse),
        (status = 500,
         description = "Database read failure while consulting the entity store.",
         body = ErrorResponse),
    ),
    tag = "Risk"
)]
pub async fn check_sanctions(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SanctionsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let chain = ChainId::new(q.chain_id.unwrap_or(ChainId::ETH.value()));
    let addr = parse_address(&q.address, chain)?;

    let result = state
        .risk()
        .check_sanctions(&addr)
        .await
        .map_err(ApiError::Internal)?;

    Ok(Json(sanctions_to_dto(&result)))
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct HeuristicsQuery {
    /// Origin wallet address to evaluate. Detection runs against the address
    /// itself and its direct counterparties already persisted in the repo.
    /// Trigger ingestion first via `POST /jobs/ingest` if the address has not
    /// been seen yet.
    #[param(example = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")]
    address: String,

    /// Chain ID. Defaults to 1 (Ethereum mainnet).
    #[param(example = 1)]
    chain_id: Option<u32>,
}

/// Evidence captured when a single behavioural-pattern heuristic fires for an
/// address. Returned as the value of any non-null field on
/// [`HeuristicsResponse`].
///
/// The shape of `addresses` and the contents of `notes` depend on which
/// heuristic fired — see the field docs.
#[derive(Serialize, utoipa::ToSchema)]
pub struct HeuristicEvidenceDto {
    /// Heuristic that fired. One of: `FanOut`, `FanIn`, `SmurfingCycle`,
    /// `TemporalBurst`, `FixedAmountClustering`, `DwellTimePassThrough`,
    /// `PeelingChain`, `DepositAddressReuse`.
    #[schema(example = "FanOut")]
    heuristic: String,

    /// Detector confidence. One of: `LOW`, `MEDIUM`, `HIGH`.
    #[schema(example = "MEDIUM")]
    confidence: String,

    /// Addresses involved in the pattern.
    /// * `FanOut`: distinct receivers seen in the burst window.
    /// * `FanIn`: distinct senders seen in the burst window.
    /// * `SmurfingCycle`: `[distributor, ...intermediaries..., cash_out]`.
    addresses: Vec<String>,

    /// Human-readable explanation including the matched counts and windows.
    #[schema(example = "5 unique receivers within a 86400s window from 0x…")]
    notes: Option<String>,
}

/// Aggregated heuristic-detection report for one address, returned by
/// `GET /heuristics`.
///
/// Each behavioural detector runs independently against the persisted
/// transfers around the subject and reports a non-null evidence object when it
/// fires. `null` means the pattern did not match (the default and most common
/// outcome).
#[derive(Serialize, utoipa::ToSchema)]
pub struct HeuristicsResponse {
    /// Echoes the canonical form of the queried address.
    address: String,

    /// Fan-out evidence — one source distributing funds to many distinct
    /// receivers inside a sliding time window. `null` if not detected.
    fan_out: Option<HeuristicEvidenceDto>,

    /// Fan-in evidence — many distinct senders converging into one
    /// address inside a sliding time window. `null` if not detected.
    fan_in: Option<HeuristicEvidenceDto>,

    /// Smurfing cycle — fan-out followed by intermediaries that route funds
    /// back into a single cash-out address within `smurf_window` and
    /// `smurf_max_depth` hops. `null` if not detected.
    smurfing_cycle: Option<HeuristicEvidenceDto>,

    /// Temporal burst — anomalous concentration of transfers (≥ `burst_min_count`
    /// in a `burst_window`, with the burst ≥ `burst_multiplier` × median baseline).
    temporal_burst: Option<HeuristicEvidenceDto>,

    /// Fixed-amount clustering — ≥ `fixed_amount_min_count` transfers around the
    /// address share the same USD bucket (`fixed_amount_bucket_usd` granularity).
    fixed_amount_clustering: Option<HeuristicEvidenceDto>,

    /// Dwell-time pass-through — median delay between matched in/out pairs
    /// is below `dwell_max_secs` over ≥ `dwell_min_pairs` matched pairs.
    dwell_time_pass_through: Option<HeuristicEvidenceDto>,

    /// Peeling chain — small slices of inflow being peeled off into a chain of
    /// short-lived addresses.
    peeling_chain: Option<HeuristicEvidenceDto>,

    /// Deposit-address reuse — many distinct senders fund the same single
    /// deposit address (exchange-style routing).
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
        (
            status = 200,
            description = "Per-heuristic detection results. Every field is either a \
                           [`HeuristicEvidenceDto`] with the matched counterparties and \
                           confidence band, or `null` when the pattern did not fire. An \
                           all-null response is normal for an unremarkable address.",
            body = HeuristicsResponse
        ),
        (status = 400,
         description = "Address failed to parse, or `chain_id` is unknown.",
         body = ErrorResponse),
        (status = 500,
         description = "Database read failure while running detectors. The error message is \
                        in the body.",
         body = ErrorResponse),
    ),
    tag = "Risk"
)]
pub async fn detect_heuristics(
    State(state): State<Arc<AppState>>,
    Query(q): Query<HeuristicsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let chain = ChainId::new(q.chain_id.unwrap_or(ChainId::ETH.value()));
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

// ── /path A->B ───────────────────────────────────────────────────────────────

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PathQuery {
    /// Source address.
    #[param(example = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")]
    from: String,
    /// Target address.
    #[param(example = "0x742d35Cc6634C0532925a3b844Bc9e7595f0bEb1")]
    to: String,
    /// Chain ID. Defaults to 1 (Ethereum mainnet).
    #[param(example = 1)]
    chain_id: Option<u32>,
    /// BFS depth limit for the auxiliary graph (default 3, see /graph).
    #[param(example = 3)]
    max_depth: Option<u32>,
    /// Node cap (default 500).
    #[param(example = 500)]
    max_nodes: Option<usize>,
}

/// A single transfer edge used to describe a hop in a shortest-path result.
///
/// Returned inside [`PathResponse`] by `GET /path`. The edges appear in
/// traversal order from `from` to `to`.
#[derive(Serialize, utoipa::ToSchema)]
pub struct PathEdgeDto {
    /// Transaction hash, hex-encoded without `0x` (64 chars on EVM).
    tx_hash: String,
    /// Sender address for this hop.
    from: String,
    /// Recipient address for this hop.
    to: String,
    /// Raw on-chain amount as a base-10 big integer string, in the asset's
    /// base units (wei for ETH).
    amount: String,
    /// Asset identifier — chain native symbol (e.g. `"ETH"`) or token symbol
    /// for ERC-20 transfers.
    asset: String,
    /// Best-effort USD value at the time of the transfer. `null` when no
    /// price was available for this asset/block.
    usd_value: Option<f64>,
    /// ISO-8601 timestamp of the block, in UTC.
    timestamp: String,
}

/// Shortest-path result returned by `GET /path`.
///
/// Describes a single chain of edges from `from` to `to` discovered inside a
/// bounded BFS subgraph anchored at `from`. When `not_found` is `true` the
/// other fields fall back to neutral defaults.
#[derive(Serialize, utoipa::ToSchema)]
pub struct PathResponse {
    /// Number of hops (== length of `edges`). 0 means `from == to`.
    length: usize,
    /// `true` when no path was found inside the bounded subgraph. When `true`,
    /// `edges` is empty and `length` is 0. A `true` here does not mean no path
    /// exists on-chain — only that none was reachable within the configured
    /// `max_depth` / `max_nodes` budget.
    not_found: bool,
    /// Hops in order from source to target. Empty when `not_found` is `true`.
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
        (status = 200,
         description = "Shortest path inside the bounded subgraph. When `not_found` is `true`, \
                        `edges` is empty and `length` is `0`. When a path is found, `edges` lists \
                        the transfers in traversal order from `from` to `to`.",
         body = PathResponse),
        (status = 400,
         description = "Either address failed to parse for the chain family, or `chain_id` is \
                        unknown.",
         body = ErrorResponse),
        (status = 500,
         description = "Database read failure while building the subgraph. The error message is \
                        included in the body.",
         body = ErrorResponse),
    ),
    tag = "Graph"
)]
pub async fn shortest_path(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PathQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let chain = ChainId::new(q.chain_id.unwrap_or(ChainId::ETH.value()));
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

// ── /cluster (Union-Find) ────────────────────────────────────────────────────

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ClusterQuery {
    #[param(example = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")]
    address: String,
    #[param(example = 1)]
    chain_id: Option<u32>,
}

/// Union-Find clustering result for an address, returned by `GET /cluster`.
///
/// A "component" is a set of addresses the detectors believe belong to the
/// same real-world owner. The queried address is always in the first component
/// (`components[0]`); additional components describe satellite clusters
/// discovered during traversal.
#[derive(Serialize, utoipa::ToSchema)]
pub struct ClusterResponse {
    /// Canonical form of the queried address.
    address: String,
    /// Connected components produced by Union-Find over the outputs of every
    /// cluster-formation detector (fan-in/out, peeling, smurfing, etc.). The
    /// queried address is guaranteed to be in `components[0]`. Each inner
    /// `Vec` is the list of canonical addresses inside that component.
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
        (status = 200,
         description = "Connected-component partition. `components[0]` always contains the \
                        queried address. An empty `components` array means no co-ownership \
                        signals were detected (rare unless ingestion produced no graph).",
         body = ClusterResponse),
        (status = 400,
         description = "Address failed to parse or `chain_id` is unknown.",
         body = ErrorResponse),
        (status = 500,
         description = "Database read or detector failure. The error is in the body.",
         body = ErrorResponse),
    ),
    tag = "Risk"
)]
pub async fn cluster_address(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ClusterQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let chain = ChainId::new(q.chain_id.unwrap_or(ChainId::ETH.value()));
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

// ── /edges/significance ─────────────────────────────────────────────────────

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EdgeSignificanceQuery {
    #[param(example = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")]
    address: String,
    #[param(example = 1)]
    chain_id: Option<u32>,
    /// How many top-scored edges to return (default 50).
    #[param(example = 50)]
    limit: Option<usize>,
}

/// One edge with its forensic-significance score, returned inside
/// [`EdgeSignificanceResponse`].
///
/// The `score` field is the heuristic significance of this specific transfer
/// relative to its neighbours — higher values indicate transfers that stand
/// out and probably deserve human attention.
#[derive(Serialize, utoipa::ToSchema)]
pub struct EdgeScoreDto {
    /// Transaction hash, hex-encoded without `0x`.
    tx_hash: String,
    /// Canonical sender address.
    from: String,
    /// Canonical recipient address.
    to: String,
    /// Raw on-chain amount in the asset's base units, as a base-10 big integer
    /// string (wei for ETH).
    amount: String,
    /// Asset identifier — chain native symbol or ERC-20 token symbol.
    asset: String,
    /// Best-effort USD value at the time of the transfer. `null` when no
    /// price was available.
    usd_value: Option<f64>,
    /// ISO-8601 timestamp of the block, UTC.
    timestamp: String,
    /// Forensic-significance score in roughly `[0.0, 1.0]`. The higher, the
    /// more this edge stands out from its neighbours. Use as a sort key when
    /// triaging large graphs.
    score: f64,
}

/// Top forensic edges around an address, returned by `GET /edges/significance`.
///
/// The edges are sorted from highest-significance to lowest. Sort order is
/// stable for ties in score.
#[derive(Serialize, utoipa::ToSchema)]
pub struct EdgeSignificanceResponse {
    /// Canonical form of the queried address (echoes the request).
    address: String,
    /// Top edges in/out of the address, sorted by descending `score`. Length
    /// is capped at the request's `limit` (default 50).
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
        (status = 200,
         description = "Top-N edges sorted by descending significance score. An empty `edges` \
                        array means no transfers have been ingested for this address yet.",
         body = EdgeSignificanceResponse),
        (status = 400,
         description = "Address failed to parse or `chain_id` is unknown.",
         body = ErrorResponse),
        (status = 500,
         description = "Database read failure. The error message is in the body.",
         body = ErrorResponse),
    ),
    tag = "Risk"
)]
pub async fn edge_significance_endpoint(
    State(state): State<Arc<AppState>>,
    Query(q): Query<EdgeSignificanceQuery>,
) -> Result<impl IntoResponse, ApiError> {
    use domain::ports::TransferRepository;
    let chain = ChainId::new(q.chain_id.unwrap_or(ChainId::ETH.value()));
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

// ── /watchlist + /alerts ─────────────────────────────────────────────────────

/// Request body for `POST /watchlist`. Adds a single address to the watchlist
/// so future ingestions automatically raise an alert when they touch it.
#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct WatchlistAddRequest {
    /// Address to watch. Must parse against the chosen `chain_id`.
    #[schema(example = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")]
    address: String,
    /// Chain id the address belongs to. Defaults to `1` (Ethereum mainnet).
    #[schema(example = 1)]
    chain_id: Option<u32>,
    /// Free-form note explaining why this address is on the watchlist. Surfaces
    /// on every alert that this address triggers. Optional.
    #[schema(example = "Suspect address from incident #42")]
    reason: Option<String>,
}

/// One entry in the watchlist. Returned by `POST /watchlist` (single) and
/// `GET /watchlist` (array).
#[derive(Serialize, utoipa::ToSchema)]
pub struct WatchlistEntryDto {
    /// Canonical form of the watched address.
    address: String,
    /// The note attached when the address was added (echo of
    /// `WatchlistAddRequest.reason`). `null` when no reason was supplied.
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
        (status = 200,
         description = "Address persisted in the watchlist. The response echoes the canonical \
                        address and reason for easy confirmation.",
         body = WatchlistEntryDto),
        (status = 400,
         description = "Address failed to parse for the chain family or `chain_id` is unknown.",
         body = ErrorResponse),
        (status = 401,
         description = "Missing or invalid `X-Admin-Api-Key`.",
         body = ErrorResponse),
    ),
    tag = "Watchlist"
)]
pub async fn watchlist_add(
    State(state): State<Arc<AppState>>,
    Json(body): Json<WatchlistAddRequest>,
) -> Result<impl IntoResponse, ApiError> {
    use domain::ports::{WatchlistEntry, WatchlistRepository};
    let chain = ChainId::new(body.chain_id.unwrap_or(ChainId::ETH.value()));
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
        (status = 200,
         description = "Every address currently on the watchlist. May be an empty array.",
         body = [WatchlistEntryDto]),
        (status = 401,
         description = "Missing or invalid `X-Admin-Api-Key`.",
         body = ErrorResponse),
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
        (status = 200,
         description = "Body: `{\"removed\": true|false}`. `true` means a row was deleted; \
                        `false` means the address was not on the watchlist (and the call was a \
                        no-op)."),
        (status = 400,
         description = "Address failed to parse for the chain family.",
         body = ErrorResponse),
        (status = 401,
         description = "Missing or invalid `X-Admin-Api-Key`.",
         body = ErrorResponse),
    ),
    tag = "Watchlist"
)]
pub async fn watchlist_remove(
    State(state): State<Arc<AppState>>,
    Query(q): Query<WatchlistRemoveQuery>,
) -> Result<impl IntoResponse, ApiError> {
    use domain::ports::WatchlistRepository;
    let chain = ChainId::new(q.chain_id.unwrap_or(ChainId::ETH.value()));
    let addr = parse_address(&q.address, chain)?;
    let removed = state.watchlist().remove(&addr).await.map_err(ApiError::Internal)?;
    Ok(Json(serde_json::json!({ "removed": removed })))
}

/// One alert raised when a watched address was touched by an ingestion run.
///
/// Returned in the array body of `GET /alerts`. Acts as an audit log entry —
/// it records the specific transfer that triggered the alert.
#[derive(Serialize, utoipa::ToSchema)]
pub struct AlertDto {
    /// Canonical form of the watched address that was hit.
    address: String,
    /// Transaction hash that touched the watched address, hex-encoded without
    /// the `0x` prefix.
    tx_hash: String,
    /// Log / event index inside the transaction. Disambiguates multiple
    /// transfers in the same tx (e.g. several ERC-20 transfers in one call).
    tx_idx: u32,
    /// ISO-8601 timestamp (UTC) when the alert was recorded.
    created_at: String,
    /// Reason copied from the watchlist entry (`WatchlistAddRequest.reason`)
    /// at the moment the alert was raised. `null` if no reason was set.
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
        (status = 200,
         description = "Every alert ever recorded by ingestion, oldest first. May be empty.",
         body = [AlertDto]),
        (status = 401,
         description = "Missing or invalid `X-Admin-Api-Key`.",
         body = ErrorResponse),
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

// ── /address/{addr}/kind ─────────────────────────────────────────────────────

/// Request body for `POST /address/{addr}/kind`. Overrides the address-kind
/// classification stored for an address.
#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct AddressKindRequest {
    /// New kind to assign. One of: `eoa` (externally-owned account),
    /// `contract` (smart contract / TRX contract), `known_service` (named
    /// custodial wallet — exchange, payment processor, etc.), or `unknown`
    /// (forget any prior classification).
    #[schema(example = "contract")]
    kind: String,
    /// Display name for `known_service`. Ignored for other kinds. Required
    /// (and non-empty) when `kind == "known_service"`.
    #[schema(example = "Binance hot wallet")]
    service_name: Option<String>,
    /// Chain id the address belongs to. Defaults to `1` (Ethereum mainnet).
    #[schema(example = 1)]
    chain_id: Option<u32>,
}

/// Address-kind classification, returned by both
/// `GET /address/{addr}/kind` and `POST /address/{addr}/kind`.
#[derive(Serialize, utoipa::ToSchema)]
pub struct AddressKindResponse {
    /// Canonical form of the address.
    address: String,
    /// Current classification. One of `eoa`, `contract`, `known_service`,
    /// `unknown`. `unknown` is returned for addresses the classifier has not
    /// yet seen.
    kind: String,
    /// Service name when `kind == "known_service"`. `null` for every other
    /// kind.
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
                   The path parameter is parsed against Ethereum mainnet (`chain_id = 1`) — \
                   non-EVM addresses use the same route only as long as their canonical form is \
                   accepted by the parser.",
    params(("addr" = String, Path, description = "Address in hex (EVM) or canonical chain form (other families).")),
    responses(
        (status = 200,
         description = "Current classification. `kind == \"unknown\"` is the default for \
                        unseen addresses; `service_name` is non-null only when \
                        `kind == \"known_service\"`.",
         body = AddressKindResponse),
        (status = 400,
         description = "Path parameter failed to parse as an address.",
         body = ErrorResponse),
    ),
    tag = "Risk"
)]
pub async fn get_address_kind(
    State(state): State<Arc<AppState>>,
    Path(addr_param): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    use domain::ports::AddressKindRepository;
    let chain = ChainId::ETH;
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
        (status = 200,
         description = "Updated classification (echoes the stored row).",
         body = AddressKindResponse),
        (status = 400,
         description = "Address failed to parse, or `kind` is not one of the allowed values.",
         body = ErrorResponse),
        (status = 401,
         description = "Missing or invalid `X-Admin-Api-Key`.",
         body = ErrorResponse),
    ),
    tag = "Risk"
)]
pub async fn set_address_kind(
    State(state): State<Arc<AppState>>,
    Path(addr_param): Path<String>,
    Json(body): Json<AddressKindRequest>,
) -> Result<impl IntoResponse, ApiError> {
    use domain::ports::AddressKindRepository;
    let chain = ChainId::new(body.chain_id.unwrap_or(ChainId::ETH.value()));
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

// ── /labels + /entities ──────────────────────────────────────────────────────

/// Request body for `POST /labels` (single) and each element of
/// `POST /labels/bulk`. Attaches a category + label to an address, creating
/// or extending the underlying entity.
#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct LabelRequest {
    /// Address to tag. EVM addresses use the `0x...` hex form; other families
    /// use their canonical chain form.
    #[schema(example = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")]
    address: String,
    /// Chain id the address belongs to. Defaults to `1` (Ethereum mainnet).
    #[schema(example = 1)]
    chain_id: Option<u32>,
    /// Entity category. One of: `exchange`, `mixer`, `bridge`, `defi`, `scam`,
    /// `gambling`, `darknet`, `mining`, `sanctioned`, `unknown`. Drives the
    /// default `risk_score` and the trace-sink classifier.
    #[schema(example = "exchange")]
    category: String,
    /// Human-readable display name for the entity (e.g. `"Binance hot wallet 14"`).
    /// When the same `(category, label_name)` already exists, the address is
    /// appended to that entity instead of creating a new one — this is how
    /// you group multiple addresses under the same logical entity.
    #[schema(example = "Binance 14")]
    label_name: Option<String>,
    /// Optional reference URL (e.g. an Etherscan link, an internal investigation
    /// ticket). Stored as-is and round-tripped in `LabelResponse.label_url`.
    #[schema(example = "https://etherscan.io/address/0xd8dA…")]
    label_url: Option<String>,
    /// Provenance of the label. One of: `manual`, `chainalysis`, `internal`,
    /// `community`. Defaults to `manual`. Affects nothing automatic — purely
    /// metadata for audit.
    #[schema(example = "manual")]
    label_source: Option<String>,
    /// Specific sanctions list. Only meaningful when `category == "sanctioned"`.
    /// One of `ofac`, `eu`, `un`, or a custom snake_case identifier.
    #[schema(example = "ofac")]
    sanction_list: Option<String>,
    /// Override for the entity's risk score (0–100). When omitted the default
    /// for the category is used: `sanctioned = 100`, `darknet = 95`,
    /// `mixer = 90`, `scam = 75`, `bridge = 40`, `exchange = 30`, others = 25.
    #[schema(example = 30)]
    risk_score: Option<u8>,
}

/// Entity record returned by every `/labels` and `/entities` endpoint.
///
/// One entity may carry many addresses (e.g. an exchange's hot-wallet set),
/// hence `addresses` is plural. Reading a single address through `GET /labels/{addr}`
/// returns the entity it belongs to together with all sibling addresses.
#[derive(Serialize, utoipa::ToSchema)]
pub struct LabelResponse {
    /// Internal UUID of the entity. Stable across address additions/removals.
    entity_id: String,
    /// Every address attached to this entity, in canonical form. Includes
    /// addresses other than the one queried — that's how you discover related
    /// addresses by labelling one of them.
    addresses: Vec<String>,
    /// Entity category. Same vocabulary as `LabelRequest.category`.
    category: String,
    /// Sanctions list name when `category == "sanctioned"`, otherwise `null`.
    sanction_list: Option<String>,
    /// Display name (echoes `LabelRequest.label_name`). `null` if never set.
    label_name: Option<String>,
    /// Reference URL (echoes `LabelRequest.label_url`). `null` if never set.
    label_url: Option<String>,
    /// Provenance of the label. One of `manual`, `chainalysis`, `internal`,
    /// `community`. `null` if no label has ever been attached.
    label_source: Option<String>,
    /// Entity-level risk score 0–100. Used by `/score` and the trace-sink
    /// classifier.
    risk_score: u8,
}

/// Result of `POST /labels/bulk`. Summarises how many entries were applied
/// successfully and which ones failed.
#[derive(Serialize, utoipa::ToSchema)]
pub struct LabelsBulkResponse {
    /// Number of entries that were upserted successfully.
    upserted: usize,
    /// Per-row error messages for entries that could not be applied. Format
    /// is `"<address>: <reason>"`. Empty when every entry succeeded.
    errors: Vec<String>,
}

fn parse_category(
    s: &str,
    sanction_list: Option<&str>,
) -> Result<domain::entity::EntityCategory, ApiError> {
    use domain::entity::{EntityCategory, SanctionList};
    Ok(match s {
        "exchange" => EntityCategory::Exchange,
        "mixer" => EntityCategory::Mixer,
        "bridge" => EntityCategory::Bridge,
        "defi" => EntityCategory::DefiProtocol,
        "scam" => EntityCategory::Scam,
        "gambling" => EntityCategory::Gambling,
        "darknet" => EntityCategory::Darknet,
        "mining" => EntityCategory::Mining,
        "unknown" => EntityCategory::Unknown,
        "sanctioned" => {
            let sl = match sanction_list.unwrap_or("ofac") {
                "ofac" => SanctionList::Ofac,
                "eu" => SanctionList::Eu,
                "un" => SanctionList::Un,
                other => SanctionList::Other(other.to_string()),
            };
            EntityCategory::Sanctioned { sanction_list: sl }
        }
        other => return Err(ApiError::bad_request(format!("unknown category: {other}"))),
    })
}

fn parse_source(s: Option<&str>) -> domain::entity::LabelSource {
    use domain::entity::LabelSource;
    match s.unwrap_or("manual") {
        "chainalysis" => LabelSource::Chainalysis,
        "internal" => LabelSource::Internal,
        "community" => LabelSource::Community,
        _ => LabelSource::Manual,
    }
}

fn default_risk_for(cat: &domain::entity::EntityCategory) -> u8 {
    use domain::entity::EntityCategory;
    match cat {
        EntityCategory::Sanctioned { .. } => 100,
        EntityCategory::Darknet => 95,
        EntityCategory::Mixer => 90,
        EntityCategory::Scam => 75,
        EntityCategory::Bridge => 40,
        EntityCategory::Exchange => 30,
        _ => 25,
    }
}

fn entity_to_dto(e: &domain::entity::Entity) -> LabelResponse {
    use domain::entity::EntityCategory;
    let (cat_s, sl) = match e.category() {
        EntityCategory::Exchange => ("exchange".to_string(), None),
        EntityCategory::Mixer => ("mixer".to_string(), None),
        EntityCategory::Bridge => ("bridge".to_string(), None),
        EntityCategory::DefiProtocol => ("defi".to_string(), None),
        EntityCategory::Scam => ("scam".to_string(), None),
        EntityCategory::Gambling => ("gambling".to_string(), None),
        EntityCategory::Darknet => ("darknet".to_string(), None),
        EntityCategory::Mining => ("mining".to_string(), None),
        EntityCategory::Unknown => ("unknown".to_string(), None),
        EntityCategory::Sanctioned { sanction_list } => (
            "sanctioned".to_string(),
            Some(match sanction_list {
                domain::entity::SanctionList::Ofac => "ofac".to_string(),
                domain::entity::SanctionList::Eu => "eu".to_string(),
                domain::entity::SanctionList::Un => "un".to_string(),
                domain::entity::SanctionList::Other(s) => s.clone(),
            }),
        ),
    };
    LabelResponse {
        entity_id: e.id().value().to_string(),
        addresses: e.addresses().iter().map(|a| a.canonical()).collect(),
        category: cat_s,
        sanction_list: sl,
        label_name: e.label().map(|l| l.name().to_string()),
        label_url: e.label().and_then(|l| l.url()).map(str::to_owned),
        label_source: e.label().map(|l| match l.source() {
            domain::entity::LabelSource::Manual => "manual".to_string(),
            domain::entity::LabelSource::Chainalysis => "chainalysis".to_string(),
            domain::entity::LabelSource::Internal => "internal".to_string(),
            domain::entity::LabelSource::Community => "community".to_string(),
        }),
        risk_score: e.risk_score().value(),
    }
}

async fn apply_label(
    state: &AppState,
    body: &LabelRequest,
) -> Result<domain::entity::Entity, ApiError> {
    use domain::entity::{Entity, EntityLabel, RiskScore};
    use domain::ports::EntityRepository;

    let chain = ChainId::new(body.chain_id.unwrap_or(ChainId::ETH.value()));
    let addr = parse_address(&body.address, chain)?;
    let category = parse_category(&body.category, body.sanction_list.as_deref())?;
    let risk = RiskScore::new(body.risk_score.unwrap_or_else(|| default_risk_for(&category)));

    // 1) Prefer the entity already attached to this address.
    let mut entity = state
        .entities()
        .find_by_address(&addr)
        .await
        .map_err(ApiError::Internal)?;

    // 2) Otherwise try to merge into an entity with the same (category, name).
    if entity.is_none() {
        if let Some(name) = body.label_name.as_deref() {
            entity = state
                .entities()
                .find_by_label(&category, name)
                .await
                .map_err(ApiError::Internal)?;
        }
    }

    // 3) Otherwise create a new entity.
    let mut entity = entity.unwrap_or_else(|| Entity::new(category.clone(), risk));

    if let Some(name) = body.label_name.clone() {
        entity.set_label(EntityLabel::new(
            name,
            body.label_url.clone(),
            parse_source(body.label_source.as_deref()),
        ));
    }
    entity.add_address(addr);

    state
        .entities()
        .save(&entity)
        .await
        .map_err(ApiError::Internal)?;

    Ok(entity)
}

#[utoipa::path(
    post, path = "/labels",
    description = "Tag an address with a category + label (admin only).\n\n\
                   ## What this does\n\n\
                   Resolves the target entity using the following priority:\n\n\
                   1. If the address is already attached to an entity, reuse that entity.\n\
                   2. Else, if `label_name` matches an existing `(category, label_name)` pair, \
                      append the address there.\n\
                   3. Else, create a brand-new entity with the supplied category + label.\n\n\
                   The resulting entity is then persisted with the address attached. This is \
                   how you build up multi-address entities (e.g. all of an exchange's hot \
                   wallets) by repeatedly labelling individual addresses with the same \
                   `label_name`.\n\n\
                   Writes go to PostgreSQL. The label is immediately visible to `/score`, \
                   `/sanctions`, and the trace-sink classifier.\n\n\
                   ## When to use it\n\n\
                   Use this to inject your own attribution data — internal investigations, \
                   curated allow-lists, or imports from third-party providers. For batch \
                   imports, prefer `POST /labels/bulk`.\n\n\
                   ## Example\n\n\
                   ```bash\n\
                   curl -X POST 'http://localhost:8080/labels' \\\n\
                     -H 'X-API-Version: 1' \\\n\
                     -H 'X-Admin-Api-Key: <admin-key>' \\\n\
                     -H 'Content-Type: application/json' \\\n\
                     -d '{\n\
                       \"address\": \"0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045\",\n\
                       \"chain_id\": 1,\n\
                       \"category\": \"exchange\",\n\
                       \"label_name\": \"Binance hot wallet 14\",\n\
                       \"label_source\": \"manual\"\n\
                     }'\n\
                   ```\n\n\
                   ## Notes\n\n\
                   Requires the `X-Admin-Api-Key` header. `risk_score` is auto-defaulted from \
                   `category` when omitted — explicit overrides only matter for fine-tuning.",
    request_body = LabelRequest,
    responses(
        (status = 200,
         description = "The resulting entity after upsert. `addresses` includes every address \
                        attached to this entity (not just the one you just added).",
         body = LabelResponse),
        (status = 400,
         description = "Address failed to parse for the chain family, `chain_id` is unknown, \
                        or `category` is not one of the allowed values.",
         body = ErrorResponse),
        (status = 401,
         description = "Missing or invalid `X-Admin-Api-Key`.",
         body = ErrorResponse),
    ),
    tag = "Labels"
)]
pub async fn labels_set(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LabelRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let entity = apply_label(&state, &body).await?;
    Ok(Json(entity_to_dto(&entity)))
}

#[utoipa::path(
    get, path = "/labels/{addr}",
    description = "Look up the entity attached to an address (admin only).\n\n\
                   ## What this does\n\n\
                   Returns the entity that contains the supplied address, including every sibling \
                   address attached to that same entity. Useful for discovering related wallets — \
                   labelling one of an exchange's hot wallets exposes the full set.\n\n\
                   This is a DB-only read. No chain RPC is touched.\n\n\
                   ## When to use it\n\n\
                   Call this to render the label / category / risk score for an address in a UI, \
                   or to expand a known address into the wider set of co-labelled siblings.\n\n\
                   ## Example\n\n\
                   ```bash\n\
                   curl 'http://localhost:8080/labels/0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045' \\\n\
                     -H 'X-API-Version: 1' \\\n\
                     -H 'X-Admin-Api-Key: <admin-key>'\n\
                   ```\n\n\
                   ## Notes\n\n\
                   Requires the `X-Admin-Api-Key` header. The path parameter is parsed against \
                   Ethereum mainnet — for other chains, attach labels via the same address \
                   format that the chain registry produces.",
    params(("addr" = String, Path, description = "Address in hex (EVM) or canonical chain form.")),
    responses(
        (status = 200,
         description = "The entity. `addresses` includes every address inside the same entity, \
                        not just the one queried.",
         body = LabelResponse),
        (status = 400,
         description = "Path parameter failed to parse as an address, OR the address has no \
                        label attached (the endpoint returns 400 — not 404 — for that case).",
         body = ErrorResponse),
        (status = 401,
         description = "Missing or invalid `X-Admin-Api-Key`.",
         body = ErrorResponse),
    ),
    tag = "Labels"
)]
pub async fn labels_get(
    State(state): State<Arc<AppState>>,
    Path(addr_param): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    use domain::ports::EntityRepository;
    let addr = parse_address(&addr_param, ChainId::ETH)?;
    let entity = state
        .entities()
        .find_by_address(&addr)
        .await
        .map_err(ApiError::Internal)?
        .ok_or_else(|| ApiError::bad_request("no label for this address".to_string()))?;
    Ok(Json(entity_to_dto(&entity)))
}

#[utoipa::path(
    delete, path = "/labels/{addr}",
    description = "Detach an address from its entity (admin only).\n\n\
                   ## What this does\n\n\
                   Removes the address from whatever entity currently holds it. The entity \
                   itself is preserved (with its remaining addresses) so other addresses in the \
                   same entity keep their label. If the address has no label attached, the call \
                   is a no-op.\n\n\
                   ## When to use it\n\n\
                   Use this when a labelling decision turns out to be wrong, or when an address \
                   is decommissioned. To delete an entity entirely, remove every address one by \
                   one — there is no separate \"delete entity\" endpoint.\n\n\
                   ## Example\n\n\
                   ```bash\n\
                   curl -X DELETE 'http://localhost:8080/labels/0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045' \\\n\
                     -H 'X-API-Version: 1' \\\n\
                     -H 'X-Admin-Api-Key: <admin-key>'\n\
                   ```\n\n\
                   ## Notes\n\n\
                   Requires the `X-Admin-Api-Key` header.",
    params(("addr" = String, Path, description = "Address in hex (EVM) or canonical chain form.")),
    responses(
        (status = 200,
         description = "Body: `{\"removed\": true, \"entity_id\": \"...\", \"remaining\": N}` \
                        when the address was attached and is now detached (`remaining` is the \
                        number of addresses still on the entity); or `{\"removed\": false}` \
                        when the address had no label."),
        (status = 400,
         description = "Path parameter failed to parse as an address.",
         body = ErrorResponse),
        (status = 401,
         description = "Missing or invalid `X-Admin-Api-Key`.",
         body = ErrorResponse),
    ),
    tag = "Labels"
)]
pub async fn labels_delete(
    State(state): State<Arc<AppState>>,
    Path(addr_param): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    use domain::entity::Entity;
    use domain::ports::EntityRepository;
    let addr = parse_address(&addr_param, ChainId::ETH)?;
    let Some(entity) = state
        .entities()
        .find_by_address(&addr)
        .await
        .map_err(ApiError::Internal)?
    else {
        return Ok(Json(serde_json::json!({ "removed": false })));
    };

    let mut addresses = entity.addresses().clone();
    addresses.remove(&addr);
    let mut updated = Entity::from_parts(
        entity.id().clone(),
        entity.label().cloned(),
        entity.category().clone(),
        addresses,
        entity.risk_score(),
    );
    if let Some(label) = entity.label() {
        updated.set_label(label.clone());
    }
    state
        .entities()
        .save(&updated)
        .await
        .map_err(ApiError::Internal)?;
    Ok(Json(serde_json::json!({
        "removed": true,
        "entity_id": entity.id().value().to_string(),
        "remaining": updated.addresses().len(),
    })))
}

#[utoipa::path(
    post, path = "/labels/bulk",
    request_body = [LabelRequest],
    responses(
        (status = 200, description = "Bulk import result", body = LabelsBulkResponse),
    ),
    tag = "Labels"
)]
pub async fn labels_bulk(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Vec<LabelRequest>>,
) -> Result<impl IntoResponse, ApiError> {
    let mut upserted = 0usize;
    let mut errors: Vec<String> = Vec::new();
    for req in body {
        match apply_label(&state, &req).await {
            Ok(_) => upserted += 1,
            Err(e) => {
                let msg = match e {
                    ApiError::BadRequest(s) => s,
                    ApiError::Unauthorized => "unauthorized".into(),
                    ApiError::Internal(de) => de.to_string(),
                    ApiError::InternalMsg(s) => s,
                };
                errors.push(format!("{}: {msg}", req.address));
            }
        }
    }
    Ok(Json(LabelsBulkResponse { upserted, errors }))
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct EntityCreateRequest {
    /// Category. Same vocabulary as `LabelRequest.category`.
    #[schema(example = "exchange")]
    category: String,
    #[schema(example = "OKX hot wallets")]
    label_name: Option<String>,
    label_url: Option<String>,
    label_source: Option<String>,
    sanction_list: Option<String>,
    risk_score: Option<u8>,
}

#[utoipa::path(
    post, path = "/entities",
    request_body = EntityCreateRequest,
    responses(
        (status = 200, description = "Entity created", body = LabelResponse),
        (status = 400, description = "Invalid category", body = ErrorResponse),
    ),
    tag = "Labels"
)]
pub async fn entity_create(
    State(state): State<Arc<AppState>>,
    Json(body): Json<EntityCreateRequest>,
) -> Result<impl IntoResponse, ApiError> {
    use domain::entity::{Entity, EntityLabel, RiskScore};
    use domain::ports::EntityRepository;
    let category = parse_category(&body.category, body.sanction_list.as_deref())?;
    let risk = RiskScore::new(body.risk_score.unwrap_or_else(|| default_risk_for(&category)));
    let mut entity = Entity::new(category, risk);
    if let Some(name) = body.label_name {
        entity.set_label(EntityLabel::new(
            name,
            body.label_url,
            parse_source(body.label_source.as_deref()),
        ));
    }
    state
        .entities()
        .save(&entity)
        .await
        .map_err(ApiError::Internal)?;
    Ok(Json(entity_to_dto(&entity)))
}

#[utoipa::path(
    get, path = "/entities/{id}",
    params(("id" = String, Path, description = "Entity UUID")),
    responses(
        (status = 200, description = "Entity details", body = LabelResponse),
        (status = 404, description = "Unknown entity_id", body = ErrorResponse),
    ),
    tag = "Labels"
)]
pub async fn entity_get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    use domain::entity::EntityId;
    use domain::ports::EntityRepository;
    let uuid = uuid::Uuid::parse_str(&id)
        .map_err(|e| ApiError::bad_request(format!("invalid uuid: {e}")))?;
    let entity = state
        .entities()
        .find_by_id(&EntityId::from_uuid(uuid))
        .await
        .map_err(ApiError::Internal)?
        .ok_or_else(|| ApiError::bad_request("unknown entity_id".to_string()))?;
    Ok(Json(entity_to_dto(&entity)))
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct EntityAddAddressesRequest {
    chain_id: Option<u32>,
    addresses: Vec<String>,
}

#[utoipa::path(
    post, path = "/entities/{id}/addresses",
    params(("id" = String, Path, description = "Entity UUID")),
    request_body = EntityAddAddressesRequest,
    responses(
        (status = 200, description = "Updated entity", body = LabelResponse),
        (status = 400, description = "Invalid address or unknown entity", body = ErrorResponse),
    ),
    tag = "Labels"
)]
pub async fn entity_add_addresses(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<EntityAddAddressesRequest>,
) -> Result<impl IntoResponse, ApiError> {
    use domain::entity::EntityId;
    use domain::ports::EntityRepository;
    let uuid = uuid::Uuid::parse_str(&id)
        .map_err(|e| ApiError::bad_request(format!("invalid uuid: {e}")))?;
    let mut entity = state
        .entities()
        .find_by_id(&EntityId::from_uuid(uuid))
        .await
        .map_err(ApiError::Internal)?
        .ok_or_else(|| ApiError::bad_request("unknown entity_id".to_string()))?;
    let chain = ChainId::new(body.chain_id.unwrap_or(ChainId::ETH.value()));
    for s in &body.addresses {
        let a = parse_address(s, chain)?;
        entity.add_address(a);
    }
    state
        .entities()
        .save(&entity)
        .await
        .map_err(ApiError::Internal)?;
    Ok(Json(entity_to_dto(&entity)))
}

#[utoipa::path(
    delete, path = "/entities/{id}/addresses/{addr}",
    params(
        ("id" = String, Path, description = "Entity UUID"),
        ("addr" = String, Path, description = "Address (hex or canonical)"),
    ),
    responses(
        (status = 200, description = "Address removed", body = LabelResponse),
        (status = 400, description = "Invalid address or unknown entity", body = ErrorResponse),
    ),
    tag = "Labels"
)]
pub async fn entity_remove_address(
    State(state): State<Arc<AppState>>,
    Path((id, addr_param)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    use domain::entity::{Entity, EntityId};
    use domain::ports::EntityRepository;
    let uuid = uuid::Uuid::parse_str(&id)
        .map_err(|e| ApiError::bad_request(format!("invalid uuid: {e}")))?;
    let entity = state
        .entities()
        .find_by_id(&EntityId::from_uuid(uuid))
        .await
        .map_err(ApiError::Internal)?
        .ok_or_else(|| ApiError::bad_request("unknown entity_id".to_string()))?;
    let addr = parse_address(&addr_param, ChainId::ETH)?;
    let mut addresses = entity.addresses().clone();
    addresses.remove(&addr);
    let mut updated = Entity::from_parts(
        entity.id().clone(),
        entity.label().cloned(),
        entity.category().clone(),
        addresses,
        entity.risk_score(),
    );
    if let Some(label) = entity.label() {
        updated.set_label(label.clone());
    }
    state
        .entities()
        .save(&updated)
        .await
        .map_err(ApiError::Internal)?;
    Ok(Json(entity_to_dto(&updated)))
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
    /// Per-edge forensic-significance threshold (0..1). Edges scoring below
    /// this are skipped during traversal. Omit to disable the filter.
    #[param(example = 0.2)]
    min_significance: Option<f64>,
}

#[utoipa::path(
    get, path = "/trace",
    params(TraceQuery),
    responses(
        (status = 200, description = "Fund trace result with paths and terminal sinks", body = TraceResponse),
        (status = 400, description = "Invalid address or parameter value", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
    ),
    tag = "Risk"
)]
pub async fn trace_funds(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TraceQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let chain = ChainId::new(q.chain_id.unwrap_or(ChainId::ETH.value()));
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

// ── Job endpoints ────────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct IngestJobRequest {
    #[schema(example = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")]
    address: String,
    #[schema(example = 1)]
    chain_id: Option<u32>,
    #[schema(example = 3)]
    max_depth: Option<u32>,
    #[schema(example = 500)]
    max_nodes: Option<usize>,
    #[schema(example = 10000)]
    max_transfers_per_address: Option<usize>,
    from_block: Option<u64>,
    to_block: Option<u64>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct JobAcceptedResponse {
    job_id: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct JobStatusResponse {
    id: String,
    status: String,
    error: Option<String>,
    created_at: String,
    updated_at: String,
}

#[utoipa::path(
    post, path = "/jobs/ingest",
    request_body = IngestJobRequest,
    responses(
        (status = 202, description = "Ingestion job accepted", body = JobAcceptedResponse),
        (status = 400, description = "Invalid address or parameters", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
    ),
    tag = "Jobs"
)]
pub async fn create_ingest_job(
    State(state): State<Arc<AppState>>,
    Json(body): Json<IngestJobRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let chain = ChainId::new(body.chain_id.unwrap_or(ChainId::ETH.value()));
    let addr = parse_address(&body.address, chain)?;

    let max_depth = body.max_depth.unwrap_or(3);
    let max_nodes = body.max_nodes.unwrap_or(500);
    let max_transfers_per_address = body.max_transfers_per_address.unwrap_or(10_000);
    let range = match (body.from_block, body.to_block) {
        (None, None) => None,
        (from, to) => Some(BlockRange::new(from.unwrap_or(0), to.unwrap_or(u64::MAX))),
    };

    let payload = serde_json::to_value(&body).map_err(|e| ApiError::InternalMsg(e.to_string()))?;

    let job_id = state
        .jobs()
        .create_job("ingest", payload)
        .await
        .map_err(ApiError::Internal)?;

    let state_for_job = Arc::clone(&state);
    let addr_for_job = addr.clone();
    tokio::spawn(async move {
        if let Err(e) = state_for_job.jobs().set_running(job_id).await {
            tracing::error!(error = %e, job_id = %job_id, "failed to mark job running");
            return;
        }
        let req = GraphRequest::new(range, max_depth, max_nodes, max_transfers_per_address);
        match state_for_job
            .ingestion()
            .build_graph(&addr_for_job, req)
            .await
        {
            Ok(_) => {
                if let Err(e) = state_for_job.jobs().set_done(job_id).await {
                    tracing::error!(error = %e, job_id = %job_id, "failed to mark job done");
                }
            }
            Err(e) => {
                let msg = e.to_string();
                tracing::warn!(error = %msg, job_id = %job_id, "ingest job failed");
                if let Err(e2) = state_for_job.jobs().set_failed(job_id, &msg).await {
                    tracing::error!(error = %e2, job_id = %job_id, "failed to mark job failed");
                }
            }
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(JobAcceptedResponse {
            job_id: job_id.to_string(),
        }),
    ))
}

#[utoipa::path(
    get, path = "/jobs/{id}",
    params(("id" = String, Path, description = "Job UUID")),
    responses(
        (status = 200, description = "Current job status", body = JobStatusResponse),
        (status = 404, description = "Job not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
    ),
    tag = "Jobs"
)]
pub async fn get_job_status(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let uuid = uuid::Uuid::parse_str(&id)
        .map_err(|e| ApiError::bad_request(format!("invalid job id: {e}")))?;

    let job = state
        .jobs()
        .get_job(uuid)
        .await
        .map_err(ApiError::Internal)?;

    let Some(job) = job else {
        return Err(ApiError::BadRequest("job not found".into()));
    };

    Ok(Json(JobStatusResponse {
        id: job.id.to_string(),
        status: job.status,
        error: job.error,
        created_at: job.created_at.to_rfc3339(),
        updated_at: job.updated_at.to_rfc3339(),
    }))
}

// ── Batch endpoints ──────────────────────────────────────────────────────────

#[derive(Deserialize, utoipa::ToSchema)]
pub struct BatchItem {
    #[schema(example = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")]
    address: String,
    #[schema(example = 1)]
    chain_id: Option<u32>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct SanctionsBatchRequest {
    items: Vec<BatchItem>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ScoreBatchRequest {
    items: Vec<BatchItem>,
}

const MAX_SANCTIONS_BATCH: usize = 100;
const MAX_SCORE_BATCH: usize = 50;

#[utoipa::path(
    post, path = "/sanctions/batch",
    request_body = SanctionsBatchRequest,
    responses(
        (status = 200, description = "Sanctions check per address", body = [SanctionsResponse]),
        (status = 400, description = "Invalid input or batch too large", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
    ),
    tag = "Risk"
)]
pub async fn sanctions_batch(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SanctionsBatchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    if body.items.len() > MAX_SANCTIONS_BATCH {
        return Err(ApiError::bad_request(format!(
            "batch too large: {} > {MAX_SANCTIONS_BATCH}",
            body.items.len()
        )));
    }

    let mut addrs: Vec<Address> = Vec::with_capacity(body.items.len());
    for it in &body.items {
        let chain = ChainId::new(it.chain_id.unwrap_or(ChainId::ETH.value()));
        addrs.push(parse_address(&it.address, chain)?);
    }

    let results = state
        .risk()
        .check_sanctions_batch(&addrs)
        .await
        .map_err(ApiError::Internal)?;

    let dtos: Vec<SanctionsResponse> = results.iter().map(sanctions_to_dto).collect();
    Ok(Json(dtos))
}

#[utoipa::path(
    post, path = "/score/batch",
    request_body = ScoreBatchRequest,
    responses(
        (status = 200, description = "Risk report per address", body = [ScoreResponse]),
        (status = 400, description = "Invalid input or batch too large", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
    ),
    tag = "Risk"
)]
pub async fn score_batch(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ScoreBatchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    if body.items.len() > MAX_SCORE_BATCH {
        return Err(ApiError::bad_request(format!(
            "batch too large: {} > {MAX_SCORE_BATCH}",
            body.items.len()
        )));
    }

    let mut addrs: Vec<Address> = Vec::with_capacity(body.items.len());
    for it in &body.items {
        let chain = ChainId::new(it.chain_id.unwrap_or(ChainId::ETH.value()));
        addrs.push(parse_address(&it.address, chain)?);
    }

    let reports = state
        .risk()
        .score_batch(&addrs)
        .await
        .map_err(ApiError::Internal)?;

    let dtos: Vec<ScoreResponse> = reports.iter().map(report_to_dto).collect();
    Ok(Json(dtos))
}
