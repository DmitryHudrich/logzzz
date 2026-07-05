use std::sync::Arc;

use domain::chain::{ChainId, ChainRegistry};
use infra::{
    ChainSources, JobRepository, PostgresAddressKinds, PostgresAlerts, PostgresEntityRepository,
    PostgresTransferRepository, PostgresWatchlist, StaticLabelProvider, StaticPriceProvider,
};
use usecase::{IngestionService, RiskService};

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
    default_chain_id: ChainId,
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
        default_chain_id: ChainId,
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
            default_chain_id,
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

    pub fn default_chain_id(&self) -> ChainId {
        self.default_chain_id
    }
}

pub fn resolve_chain_id(state: &AppState, requested: Option<u32>) -> ChainId {
    match requested {
        Some(v) => ChainId::new(v),
        None => {
            let default = state.default_chain_id();
            tracing::warn!(
                default_chain_id = default.value(),
                "request omitted chain_id; falling back to configured default"
            );
            default
        }
    }
}
