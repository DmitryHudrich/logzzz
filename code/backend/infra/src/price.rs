use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::collections::HashMap;

use domain::asset::{AssetId, AssetKind};
use domain::chain::ChainId;
use domain::error::DomainResult;
use domain::ports::PricePort;
use domain::price::UnitPrice;
use domain::primitives::Address;

/// In-memory price provider seeded with a few common assets. Suitable for a
/// baseline MVP; meant to be swapped for a real provider (CoinGecko, BigQuery,
/// internal feed) without changing call sites.
///
/// Pricing is per-asset constant — adequate for stablecoins and for putting
/// ETH/TRX into a comparable USD bucket while a historical feed is wired up.
pub struct StaticPriceProvider {
    by_asset: HashMap<AssetId, f64>,
}

impl StaticPriceProvider {
    /// Seed with Ethereum native ($2000) and the two common stablecoins
    /// USDT/USDC ($1). Add more via `with_asset`.
    pub fn with_defaults() -> Self {
        let mut by_asset: HashMap<AssetId, f64> = HashMap::new();
        by_asset.insert(AssetId::native(ChainId::ETH), 2000.0);
        by_asset.insert(AssetId::native(ChainId::TRON), 0.12);

        // USDT, USDC on Ethereum mainnet.
        let usdt = hex::decode("dac17f958d2ee523a2206206994597c13d831ec7").unwrap();
        let usdc = hex::decode("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48").unwrap();
        by_asset.insert(AssetId::contract(ChainId::ETH, usdt), 1.0);
        by_asset.insert(AssetId::contract(ChainId::ETH, usdc), 1.0);

        // USDT-TRC20 — by far the most heavily used Tron asset; without this
        // every TRC20-USDT transfer gets `usd_value = None` and silently
        // drops out of every USD-denominated heuristic (fixed-amount
        // clustering, edge_significance's usd_weight, ...).
        let usdt_trc20 = Address::parse(ChainId::TRON, "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t")
            .expect("hardcoded Tron USDT address must parse")
            .bytes()
            .to_vec();
        by_asset.insert(AssetId::contract(ChainId::TRON, usdt_trc20), 1.0);

        Self { by_asset }
    }

    pub fn with_asset(mut self, asset: AssetId, price_usd: f64) -> Self {
        self.by_asset.insert(asset, price_usd);
        self
    }
}

#[async_trait]
impl PricePort for StaticPriceProvider {
    async fn price_at(
        &self,
        asset: &AssetId,
        _timestamp: DateTime<Utc>,
    ) -> DomainResult<Option<UnitPrice>> {
        // Stablecoin shortcut: contract-bytes-equality already covers it.
        if let Some(p) = self.by_asset.get(asset) {
            return Ok(Some(UnitPrice(*p)));
        }
        // Unknown ERC-20 → fall back to None; callers decide whether to treat
        // as 0 USD or skip the transfer.
        if matches!(asset.kind(), AssetKind::Contract(_)) {
            return Ok(None);
        }
        Ok(None)
    }
}

/// Helper used by ingestion: enrich transfers in place using `prices`. Skips
/// transfers whose asset has no listed price (leaves `usd_value = None`).
pub async fn enrich_with_usd<P: PricePort + ?Sized>(
    prices: &P,
    transfers: &mut [domain::transfer::Transfer],
) -> DomainResult<()> {
    let mut cache: HashMap<(AssetId, i64), Option<UnitPrice>> = HashMap::new();
    for t in transfers.iter_mut() {
        let key = (t.asset().clone(), t.timestamp().timestamp() / 86_400);
        let entry = match cache.get(&key) {
            Some(v) => *v,
            None => {
                let v = prices.price_at(t.asset(), t.timestamp()).await?;
                cache.insert(key, v);
                v
            }
        };
        if let Some(p) = entry {
            t.set_usd_value(p.apply(t.amount()));
        }
    }
    Ok(())
}

