use utoipa::OpenApi;

use crate::error::ErrorResponse;
use crate::handlers::address_kind::{
    AddressKindRequest, AddressKindResponse, __path_get_address_kind, __path_set_address_kind,
};
use crate::handlers::batch::{
    BatchItem, SanctionsBatchRequest, ScoreBatchRequest, __path_sanctions_batch,
    __path_score_batch,
};
use crate::handlers::chains::{ChainDto, ChainsResponse, __path_list_chains};
use crate::handlers::cluster::{ClusterResponse, __path_cluster_address};
use crate::handlers::edges::{
    EdgeScoreDto, EdgeSignificanceResponse, __path_edge_significance_endpoint,
};
use crate::handlers::graph::{EdgeDto, GraphPage, __path_get_graph};
use crate::handlers::heuristics::{
    HeuristicEvidenceDto, HeuristicsResponse, __path_detect_heuristics,
};
use crate::handlers::jobs::{
    IngestJobRequest, JobAcceptedResponse, JobStatusResponse, __path_create_ingest_job,
    __path_get_job_status,
};
use crate::handlers::labels::{
    LabelRequest, LabelResponse, LabelsBulkResponse, __path_labels_bulk, __path_labels_delete,
    __path_labels_get, __path_labels_set,
};
use crate::handlers::path::{PathEdgeDto, PathResponse, __path_shortest_path};
use crate::handlers::sanctions::{SanctionsResponse, __path_check_sanctions};
use crate::handlers::score::{ScoreResponse, SignalDto, __path_score_address};
use crate::handlers::trace::{
    PathDto, SinkDto, TraceResponse, TraceStatsDto, __path_trace_funds,
};
use crate::handlers::watchlist::{
    AlertDto, WatchlistAddRequest, WatchlistEntryDto, __path_list_alerts, __path_watchlist_add,
    __path_watchlist_list, __path_watchlist_remove,
};

pub struct ApiSecurity;

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
                       | `X-Admin-Api-Key` | Set on the server (optional) | Mutation endpoints: `POST /labels`, `POST /labels/bulk`, `DELETE /labels/{addr}`, `POST /watchlist`, `POST /address/.../kind` |\n\
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
    ))
)]
pub struct ApiDoc;
