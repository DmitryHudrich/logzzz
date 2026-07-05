use async_trait::async_trait;
use deadpool_postgres::Pool;
use std::collections::HashSet;

use domain::chain::ChainId;
use domain::entity::{
    Entity, EntityCategory, EntityId, EntityLabel, LabelSource, RiskScore, SanctionList,
};
use domain::error::{DomainError, DomainResult};
use domain::ports::EntityRepository;
use domain::primitives::Address;

pub mod alchemy_evm_source;
pub mod bigquery_evm_source;
pub mod chain_sources;
pub mod etherscan_evm_source;
pub mod evm_routed_source;
pub mod fetch_wallet_api;
pub mod forensics_repo;
pub mod in_memory;
mod job_repo;
pub mod key_pool;
pub mod pg;
pub mod price;
pub mod rate_limiter;
pub mod tron_source;
mod transfer_repo;

pub use alchemy_evm_source::{AlchemyEvmConfig, AlchemyEvmSource};
pub use bigquery_evm_source::{BigQueryEvmConfig, BigQueryEvmSource};
pub use chain_sources::{ChainSources, ChainSourcesBuilder};
pub use etherscan_evm_source::{EtherscanEvmConfig, EtherscanEvmSource};
pub use evm_routed_source::{
    Capability, RoutedChains, RoutedEvmSource, RoutedEvmSourceBuilder,
};
pub use forensics_repo::{PostgresAddressKinds, PostgresAlerts, PostgresWatchlist};
pub use in_memory::{
    InMemoryAddressKinds, InMemoryAlerts, InMemoryWatchlist, StaticLabelProvider,
    check_watchlist_and_alert,
};
pub use job_repo::{JobRepository, JobRow};
pub use price::{StaticPriceProvider, enrich_with_usd};
pub use transfer_repo::PostgresTransferRepository;
pub use tron_source::{TronGridConfig, TronGridSource};

pub struct PostgresEntityRepository {
    pool: Pool,
}

impl PostgresEntityRepository {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    pub fn clone_with_pool(&self) -> Self {
        Self {
            pool: self.pool.clone(),
        }
    }
}

impl Clone for PostgresEntityRepository {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
        }
    }
}

#[async_trait]
impl EntityRepository for PostgresEntityRepository {
    async fn find_by_id(&self, id: &EntityId) -> DomainResult<Option<Entity>> {
        let client = self.pool.get().await.map_err(pool_err)?;

        let row = client
            .query_opt(
                "SELECT e.id, e.category, e.sanction_list, e.label_name, e.label_url,
                        e.label_source, e.risk_score
                 FROM entities e
                 WHERE e.id = $1",
                &[&id.value()],
            )
            .await
            .map_err(pg_err)?;

        let Some(row) = row else { return Ok(None) };
        let entity_id = EntityId::from_uuid(row.get("id"));
        let addresses = load_addresses(&client, &entity_id).await?;
        Ok(Some(row_to_entity(&row, entity_id, addresses)?))
    }

    async fn find_by_address(&self, addr: &Address) -> DomainResult<Option<Entity>> {
        let client = self.pool.get().await.map_err(pool_err)?;

        let addr_hex = hex::encode(addr.bytes());
        let chain_id = addr.chain().value() as i32;

        let row = client
            .query_opt(
                "SELECT e.id, e.category, e.sanction_list, e.label_name, e.label_url,
                        e.label_source, e.risk_score
                 FROM entities e
                 JOIN entity_addresses ea ON ea.entity_id = e.id
                 WHERE ea.chain_id = $1 AND ea.address = $2
                 LIMIT 1",
                &[&chain_id, &addr_hex],
            )
            .await
            .map_err(pg_err)?;

        let Some(row) = row else { return Ok(None) };
        let entity_id = EntityId::from_uuid(row.get("id"));
        let addresses = load_addresses(&client, &entity_id).await?;
        Ok(Some(row_to_entity(&row, entity_id, addresses)?))
    }

    async fn find_by_label(
        &self,
        category: &EntityCategory,
        label_name: &str,
    ) -> DomainResult<Option<Entity>> {
        let client = self.pool.get().await.map_err(pool_err)?;
        let (category_s, sanction_list_s) = category_to_str(category);
        let row = client
            .query_opt(
                "SELECT e.id, e.category, e.sanction_list, e.label_name, e.label_url,
                        e.label_source, e.risk_score
                 FROM entities e
                 WHERE e.category = $1
                   AND COALESCE(e.sanction_list, '') = COALESCE($2, '')
                   AND e.label_name = $3
                 LIMIT 1",
                &[&category_s, &sanction_list_s, &label_name],
            )
            .await
            .map_err(pg_err)?;
        let Some(row) = row else { return Ok(None) };
        let entity_id = EntityId::from_uuid(row.get("id"));
        let addresses = load_addresses(&client, &entity_id).await?;
        Ok(Some(row_to_entity(&row, entity_id, addresses)?))
    }

    async fn save(&self, entity: &Entity) -> DomainResult<()> {
        let client = self.pool.get().await.map_err(pool_err)?;

        let (category_s, sanction_list_s) = category_to_str(entity.category());
        let label_name = entity.label().map(|l| l.name());
        let label_url = entity.label().and_then(|l| l.url());
        let label_source = entity.label().map(|l| label_source_str(l.source()));

        client
            .execute(
                "INSERT INTO entities (id, category, sanction_list, label_name, label_url,
                                       label_source, risk_score)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)
                 ON CONFLICT (id) DO UPDATE
                    SET category     = EXCLUDED.category,
                        sanction_list = EXCLUDED.sanction_list,
                        label_name   = EXCLUDED.label_name,
                        label_url    = EXCLUDED.label_url,
                        label_source = EXCLUDED.label_source,
                        risk_score   = EXCLUDED.risk_score",
                &[
                    &entity.id().value(),
                    &category_s,
                    &sanction_list_s,
                    &label_name,
                    &label_url,
                    &label_source,
                    &(entity.risk_score().value() as i16),
                ],
            )
            .await
            .map_err(pg_err)?;

        client
            .execute(
                "DELETE FROM entity_addresses WHERE entity_id = $1",
                &[&entity.id().value()],
            )
            .await
            .map_err(pg_err)?;

        if !entity.addresses().is_empty() {
            let entity_ids: Vec<uuid::Uuid> = entity
                .addresses()
                .iter()
                .map(|_| entity.id().value())
                .collect();
            let chain_ids: Vec<i32> = entity
                .addresses()
                .iter()
                .map(|a| a.chain().value() as i32)
                .collect();
            let addrs: Vec<String> = entity
                .addresses()
                .iter()
                .map(|a| hex::encode(a.bytes()))
                .collect();

            client
                .execute(
                    "INSERT INTO entity_addresses (entity_id, chain_id, address)
                     SELECT * FROM UNNEST($1::UUID[], $2::INT[], $3::TEXT[])
                     ON CONFLICT DO NOTHING",
                    &[&entity_ids, &chain_ids, &addrs],
                )
                .await
                .map_err(pg_err)?;
        }

        Ok(())
    }

    async fn list_sanctioned(&self) -> DomainResult<Vec<Entity>> {
        let client = self.pool.get().await.map_err(pool_err)?;

        let rows = client
            .query(
                "SELECT id, category, sanction_list, label_name, label_url,
                        label_source, risk_score
                 FROM entities
                 WHERE category = 'sanctioned'",
                &[],
            )
            .await
            .map_err(pg_err)?;

        let mut entities = Vec::with_capacity(rows.len());
        for row in &rows {
            let entity_id = EntityId::from_uuid(row.get("id"));
            let addresses = load_addresses(&client, &entity_id).await?;
            entities.push(row_to_entity(row, entity_id, addresses)?);
        }
        Ok(entities)
    }
}

async fn load_addresses(
    client: &deadpool_postgres::Object,
    entity_id: &EntityId,
) -> DomainResult<HashSet<Address>> {
    let rows = client
        .query(
            "SELECT chain_id, address FROM entity_addresses WHERE entity_id = $1",
            &[&entity_id.value()],
        )
        .await
        .map_err(pg_err)?;

    rows.iter()
        .map(|r| {
            let chain = ChainId::new(r.get::<_, i32>("chain_id") as u32);
            let bytes = hex::decode(r.get::<_, &str>("address"))
                .map_err(|e| DomainError::InsufficientData(format!("address hex: {e}")))?;
            Ok(Address::new(chain, bytes))
        })
        .collect()
}

fn row_to_entity(
    row: &tokio_postgres::Row,
    entity_id: EntityId,
    addresses: HashSet<Address>,
) -> DomainResult<Entity> {
    let category = str_to_category(row.get("category"), row.get("sanction_list"))?;

    let label = row.get::<_, Option<&str>>("label_name").map(|name| {
        EntityLabel::new(
            name.to_string(),
            row.get::<_, Option<&str>>("label_url").map(str::to_string),
            str_to_label_source(row.get("label_source")),
        )
    });

    let risk_score = RiskScore::new(row.get::<_, i16>("risk_score") as u8);

    Ok(Entity::from_parts(
        entity_id, label, category, addresses, risk_score,
    ))
}

fn category_to_str(cat: &EntityCategory) -> (&'static str, Option<&'static str>) {
    match cat {
        EntityCategory::Exchange => ("exchange", None),
        EntityCategory::Mixer => ("mixer", None),
        EntityCategory::Bridge => ("bridge", None),
        EntityCategory::DefiProtocol => ("defi", None),
        EntityCategory::Scam => ("scam", None),
        EntityCategory::Gambling => ("gambling", None),
        EntityCategory::Darknet => ("darknet", None),
        EntityCategory::Mining => ("mining", None),
        EntityCategory::Unknown => ("unknown", None),
        EntityCategory::Sanctioned { sanction_list } => (
            "sanctioned",
            Some(match sanction_list {
                SanctionList::Ofac => "ofac",
                SanctionList::Eu => "eu",
                SanctionList::Un => "un",
                SanctionList::Other(_) => "other",
            }),
        ),
    }
}

fn str_to_category(s: &str, sanction_list: Option<&str>) -> DomainResult<EntityCategory> {
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
        "sanctioned" => EntityCategory::Sanctioned {
            sanction_list: match sanction_list.unwrap_or("other") {
                "ofac" => SanctionList::Ofac,
                "eu" => SanctionList::Eu,
                "un" => SanctionList::Un,
                other => SanctionList::Other(other.to_string()),
            },
        },
        other => {
            return Err(DomainError::InsufficientData(format!(
                "unknown category: {other}"
            )));
        }
    })
}

fn label_source_str(s: &LabelSource) -> &'static str {
    match s {
        LabelSource::Manual => "manual",
        LabelSource::Chainalysis => "chainalysis",
        LabelSource::Internal => "internal",
        LabelSource::Community => "community",
    }
}

fn str_to_label_source(s: Option<&str>) -> LabelSource {
    match s.unwrap_or("manual") {
        "chainalysis" => LabelSource::Chainalysis,
        "internal" => LabelSource::Internal,
        "community" => LabelSource::Community,
        _ => LabelSource::Manual,
    }
}

pub(crate) fn pg_err(e: tokio_postgres::Error) -> DomainError {
    let detail = e
        .as_db_error()
        .map(|db| {
            let mut msg = db.message().to_string();
            if let Some(d) = db.detail() {
                msg.push_str(&format!(": {d}"));
            }
            if let Some(h) = db.hint() {
                msg.push_str(&format!(" (hint: {h})"));
            }
            msg
        })
        .unwrap_or_else(|| e.to_string());
    DomainError::InsufficientData(detail)
}

pub(crate) fn pool_err(e: deadpool_postgres::PoolError) -> DomainError {
    DomainError::InsufficientData(e.to_string())
}
