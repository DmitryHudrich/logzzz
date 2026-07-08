use crate::asset::{AssetId, AssetMeta};
use crate::chain::ChainId;
use crate::entity::{ClusterEvidence, Entity, EntityCategory, EntityId};
use crate::error::DomainResult;
use crate::graph::{GraphRequest, TransferGraph};
use crate::primitives::{Address, BlockRef};
use crate::risk::{RiskReport, SanctionsCheckResult};
use crate::trace::{TraceRequest, TraceResult};
use crate::transfer::{NormalizedBlock, Transfer};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::sync::Arc;

#[derive(Debug, Clone, Copy)]
pub struct BlockRange {
    from_height: u64,
    to_height: u64,
    time_window: Option<(DateTime<Utc>, DateTime<Utc>)>,
}

impl BlockRange {
    pub fn new(from_height: u64, to_height: u64) -> Self {
        assert!(from_height <= to_height);
        Self {
            from_height,
            to_height,
            time_window: None,
        }
    }

    pub fn single(height: u64) -> Self {
        Self::new(height, height)
    }

    pub fn full() -> Self {
        Self::new(0, u64::MAX)
    }

    pub fn with_time_window(mut self, from: DateTime<Utc>, to: DateTime<Utc>) -> Self {
        assert!(from <= to);
        self.time_window = Some((from, to));
        self
    }

    pub fn from_height(&self) -> u64 {
        self.from_height
    }

    pub fn to_height(&self) -> u64 {
        self.to_height
    }

    pub fn time_window(&self) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
        self.time_window
    }

    pub fn is_unbounded(&self) -> bool {
        self.from_height == 0 && self.to_height == u64::MAX && self.time_window.is_none()
    }
}

impl Default for BlockRange {
    fn default() -> Self {
        Self::full()
    }
}

/// Caching contract for implementors:
/// blocks with height `> latest − ChainMeta::confirmation_depth` may still be
/// reorganized. Implementations that cache pages SHOULD either skip caching
/// such pages, use a short TTL, or read only finalized data (e.g. TRON's
/// `only_confirmed=true`). Mixing finalized and unfinalized data in a long-TTL
/// cache will leak stale state across reorgs.
#[async_trait]
pub trait ChainSource: Send + Sync {
    fn chain_id(&self) -> ChainId;
    async fn latest_block(&self) -> DomainResult<BlockRef>;
    async fn fetch_block(&self, height: u64) -> DomainResult<NormalizedBlock>;
    async fn transfers_for_address(
        &self,
        addr: &Address,
        range: BlockRange,
        max_transfers: usize,
    ) -> DomainResult<Vec<Transfer>>;

    /// Whether `addr` is a smart contract (i.e. has non-empty code at the
    /// latest block). `Ok(None)` means the underlying source does not expose
    /// this information; callers should fall back to `AddressKind::Unknown`.
    async fn is_contract(&self, _addr: &Address) -> DomainResult<Option<bool>> {
        Ok(None)
    }

    /// Bulk variant of `is_contract`. Returns one entry per input address
    /// in matching order. Default impl fans out N concurrent `is_contract`
    /// calls — useful even for sources without a real batch API, since it
    /// parallelises the work that would otherwise be serial. Sources with
    /// a true bulk path (Alchemy's JSON-RPC batched requests) should
    /// override for a single round-trip per N addresses.
    async fn is_contract_batch(
        &self,
        addrs: &[Address],
    ) -> DomainResult<Vec<Option<bool>>> {
        use futures::future::join_all;
        let futs = addrs.iter().map(|a| self.is_contract(a));
        let results = join_all(futs).await;
        let mut out = Vec::with_capacity(results.len());
        for r in results {
            match r {
                Ok(v) => out.push(v),
                // Surface the first hard error so the caller can decide
                // (router treats RateLimited as failover trigger, etc).
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }
}

#[async_trait]
pub trait ChainSourceRegistry: Send + Sync {
    fn source(&self, chain: ChainId) -> Option<Arc<dyn ChainSource>>;
    fn supported_chains(&self) -> Vec<ChainId>;
}

/// Keyset cursor for paginating transfers ordered by `(block_height, idx)`.
/// Repository implementations return rows strictly greater than this tuple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferCursor {
    pub block_height: u64,
    pub idx: u32,
}

#[async_trait]
pub trait TransferRepository: Send + Sync {
    async fn save(&self, transfers: &[Transfer]) -> DomainResult<()>;

    /// Paginated read of transfers touching `addr` (either side) within `range`.
    /// Rows are returned ordered by `(block_height, idx)` strictly greater than
    /// `after`. At most `limit` rows are returned; a returned page smaller than
    /// `limit` signals end-of-stream.
    async fn find_by_address(
        &self,
        addr: &Address,
        range: Option<BlockRange>,
        after: Option<TransferCursor>,
        limit: usize,
    ) -> DomainResult<Vec<Transfer>>;
    async fn find_by_tx(&self, chain: ChainId, tx_hash: &[u8; 32]) -> DomainResult<Vec<Transfer>>;
    async fn find_outgoing(
        &self,
        addr: &Address,
        after: Option<DateTime<Utc>>,
    ) -> DomainResult<Vec<Transfer>>;
    async fn find_incoming(
        &self,
        addr: &Address,
        after: Option<DateTime<Utc>>,
    ) -> DomainResult<Vec<Transfer>>;

    /// Lowest block height already persisted for transfers touching `addr`.
    /// Used by incremental ingest to detect prefix gaps (user widens
    /// `from_block` below what was previously fetched).
    async fn min_block_height(&self, addr: &Address) -> DomainResult<Option<u64>>;

    /// Highest block height already persisted for transfers touching `addr`.
    /// Used by incremental ingest to size the refresh window.
    async fn max_block_height(&self, addr: &Address) -> DomainResult<Option<u64>>;

    /// Remove all transfers touching `addr` whose `block_height` is inside
    /// `[from_block, to_block]`. Used to drop reorged tail before re-saving.
    /// Returns the number of rows deleted.
    async fn delete_in_range(
        &self,
        addr: &Address,
        from_block: u64,
        to_block: u64,
    ) -> DomainResult<u64>;
}

#[async_trait]
pub trait EntityRepository: Send + Sync {
    async fn find_by_id(&self, id: &EntityId) -> DomainResult<Option<Entity>>;
    async fn find_by_address(&self, addr: &Address) -> DomainResult<Option<Entity>>;
    /// Find an entity by exact `(category, label.name)` match — used by the
    /// labels API to merge a new address into an existing labelled cluster.
    async fn find_by_label(
        &self,
        category: &EntityCategory,
        label_name: &str,
    ) -> DomainResult<Option<Entity>>;
    async fn save(&self, entity: &Entity) -> DomainResult<()>;
    async fn list_sanctioned(&self) -> DomainResult<Vec<Entity>>;
}

#[async_trait]
pub trait AssetRepository: Send + Sync {
    async fn find(&self, id: &AssetId) -> DomainResult<Option<AssetMeta>>;
    async fn save(&self, meta: &AssetMeta) -> DomainResult<()>;
}

#[async_trait]
pub trait LabelProvider: Send + Sync {
    async fn resolve(&self, addr: &Address) -> DomainResult<Option<crate::entity::EntityLabel>>;
}

#[async_trait]
pub trait AddressKindRepository: Send + Sync {
    async fn kind(&self, addr: &Address) -> DomainResult<crate::entity::AddressKind>;
    async fn set_kind(
        &self,
        addr: &Address,
        kind: crate::entity::AddressKind,
    ) -> DomainResult<()>;

    /// Bulk read of address kinds. Returns one entry per input address in
    /// the same order; missing rows surface as `AddressKind::Unknown` so
    /// callers don't need to special-case absences.
    ///
    /// Default impl falls back to N single calls so existing implementors
    /// keep working. Backends with a fast bulk path (Postgres ANY(...))
    /// should override.
    async fn kind_batch(
        &self,
        addrs: &[Address],
    ) -> DomainResult<Vec<crate::entity::AddressKind>> {
        let mut out = Vec::with_capacity(addrs.len());
        for a in addrs {
            out.push(self.kind(a).await?);
        }
        Ok(out)
    }

    /// Bulk upsert. Default impl loops over `set_kind`; Postgres impl
    /// pushes all rows in a single statement with UNNEST.
    async fn set_kind_batch(
        &self,
        entries: &[(Address, crate::entity::AddressKind)],
    ) -> DomainResult<()> {
        for (a, k) in entries {
            self.set_kind(a, k.clone()).await?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct WatchlistEntry {
    pub address: Address,
    pub reason: Option<String>,
}

#[async_trait]
pub trait WatchlistRepository: Send + Sync {
    async fn add(&self, entry: WatchlistEntry) -> DomainResult<()>;
    async fn remove(&self, addr: &Address) -> DomainResult<bool>;
    async fn list(&self) -> DomainResult<Vec<WatchlistEntry>>;
    async fn contains(&self, addr: &Address) -> DomainResult<bool>;
}

#[derive(Debug, Clone)]
pub struct Alert {
    pub address: Address,
    pub triggered_by_tx: [u8; 32],
    pub triggered_by_idx: u32,
    pub created_at: DateTime<Utc>,
    pub reason: Option<String>,
}

#[async_trait]
pub trait AlertSink: Send + Sync {
    async fn record(&self, alert: Alert) -> DomainResult<()>;
    async fn list(&self) -> DomainResult<Vec<Alert>>;
}

/// Historical USD pricing for `asset` at `timestamp`. Implementations should
/// cache and may return `None` if pricing is unknown (caller decides whether
/// to treat that as 0 or skip).
#[async_trait]
pub trait PricePort: Send + Sync {
    async fn price_at(
        &self,
        asset: &AssetId,
        timestamp: DateTime<Utc>,
    ) -> DomainResult<Option<crate::price::UnitPrice>>;
}

/// Application port covering data ingestion and transfer-graph construction.
#[async_trait]
pub trait IngestionPort: Send + Sync {
    async fn build_graph(
        &self,
        origin: &Address,
        req: GraphRequest,
    ) -> DomainResult<TransferGraph>;
}

/// Application port covering risk analysis: tracing, scoring, sanctions and clustering.
#[async_trait]
pub trait RiskPort: Send + Sync {
    async fn trace(&self, req: TraceRequest) -> DomainResult<TraceResult>;

    async fn score(&self, addr: &Address) -> DomainResult<RiskReport>;

    async fn check_sanctions(&self, addr: &Address) -> DomainResult<SanctionsCheckResult>;

    async fn check_sanctions_batch(
        &self,
        addrs: &[Address],
    ) -> DomainResult<Vec<SanctionsCheckResult>>;

    async fn deposit_reuse_cluster(
        &self,
        deposit_addr: &Address,
    ) -> DomainResult<Option<ClusterEvidence>>;

    async fn detect_peeling_chain(
        &self,
        addr: &Address,
    ) -> DomainResult<Option<ClusterEvidence>>;

    async fn detect_fan_out(
        &self,
        addr: &Address,
    ) -> DomainResult<Option<ClusterEvidence>>;

    async fn detect_fan_in(
        &self,
        addr: &Address,
    ) -> DomainResult<Option<ClusterEvidence>>;

    async fn detect_smurfing_cycle(
        &self,
        addr: &Address,
    ) -> DomainResult<Option<ClusterEvidence>>;

    async fn detect_temporal_burst(
        &self,
        addr: &Address,
    ) -> DomainResult<Option<ClusterEvidence>>;

    async fn detect_fixed_amount_clustering(
        &self,
        addr: &Address,
    ) -> DomainResult<Option<ClusterEvidence>>;

    async fn detect_dwell_time(
        &self,
        addr: &Address,
    ) -> DomainResult<Option<ClusterEvidence>>;

    /// Cluster all evidence under union-find: addresses appearing in the same
    /// detector output get merged into one component. Returns components as
    /// flat address lists.
    async fn cluster_address(
        &self,
        addr: &Address,
    ) -> DomainResult<Vec<Vec<Address>>>;
}
