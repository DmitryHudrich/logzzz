use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, anyhow};
use async_trait::async_trait;
use chrono::TimeZone;
use moka::future::Cache;

use domain::{
    asset::{AssetId, TokenStandard},
    chain::ChainId,
    entity::{EntityLabel, LabelSource},
    error::{DomainError, DomainResult},
    ports::{BlockRange, ChainSource, LabelProvider},
    primitives::{Address, Amount, BlockRef, TxRef, U256},
    transfer::{Finality, NormalizedBlock, Transfer, TransferId, TransferKind},
};

use crate::fetch_wallet_api::side_api::tron::{dto, endpoints};

const TRX_DECIMALS: u8 = 6;
const PAGE_SIZE: u32 = 200;

#[derive(Debug, Clone)]
pub struct TronGridConfig {
    base_url: String,
    api_key: Option<String>,
    page_cache_max_capacity: u64,
    page_cache_ttl: Duration,
    file_cache_dir: Option<PathBuf>,
    max_pages_per_endpoint: u32,
}

impl TronGridConfig {
    pub fn new(
        base_url: String,
        api_key: Option<String>,
        page_cache_max_capacity: u64,
        page_cache_ttl: Duration,
        file_cache_dir: Option<PathBuf>,
        max_pages_per_endpoint: u32,
    ) -> Self {
        Self {
            base_url,
            api_key,
            page_cache_max_capacity,
            page_cache_ttl,
            file_cache_dir,
            max_pages_per_endpoint,
        }
    }

    /// Per-chain endpoint override. The default is Tronscan's public API
    /// host; point this at a self-hosted mirror if you have one.
    pub fn with_base_url(mut self, url: String) -> Self {
        self.base_url = url;
        self
    }
}

impl Default for TronGridConfig {
    fn default() -> Self {
        Self {
            base_url: "https://apilist.tronscanapi.com".into(),
            api_key: None,
            page_cache_max_capacity: 10_000,
            page_cache_ttl: Duration::from_secs(60 * 60),
            file_cache_dir: None,
            max_pages_per_endpoint: 50,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Endpoint {
    Native,
    Trc20,
}

impl Endpoint {
    fn prefix(&self) -> &'static str {
        match self {
            Endpoint::Native => "tron_native",
            Endpoint::Trc20 => "tron_trc20",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PageKey {
    endpoint: Endpoint,
    address_b58: String,
    start: u32,
    /// The `[min_ts, max_ts]` window this page was fetched under (ms since
    /// epoch). Included in the key because the same `start` offset means a
    /// different slice of results under a different window — e.g. the first
    /// page (`start = 0`) of a full-history fetch is not the same response
    /// as the first page of a narrow hot-tail refetch.
    min_ts: Option<u64>,
    max_ts: Option<u64>,
}

/// `(rows, is_last_page)` — `is_last_page` is true once Tronscan returns
/// fewer rows than requested, which is the only reliable end-of-stream
/// signal offset pagination gives us (no opaque cursor to check for `None`).
type PageValue = Arc<(Vec<Transfer>, bool)>;

/// Reads only solidified (`confirm=true`) data — see
/// `side_api/tron/endpoints.rs`. TRON DPoS finality means solidified blocks
/// never reorg, so the hot-tail problem (relevant for ETH/Moralis) doesn't
/// apply here and pages can be cached aggressively with a single TTL, and
/// once fetched a page never needs invalidating regardless of which window
/// it was fetched under.
///
/// ## Incremental fetch and the "height" convention
///
/// Tronscan's REST API has no concept of filtering by block height — the
/// account-transaction endpoints only support `start_timestamp`/
/// `end_timestamp` (plus `start`/`limit` offset pagination). So
/// `transfers_for_address` treats the `BlockRange` it's given as a
/// **millisecond-since-epoch window** rather than a block-height window:
/// `range.from_height()`/`to_height()` are read as timestamps, and every
/// `Transfer`/`BlockRef` this source produces (including `latest_block()`)
/// carries `block_timestamp` in the `height` field, not the real Tron block
/// number. This lets the fully generic incremental-refetch logic in
/// `usecase::ingestion` (which is written in terms of
/// `TransferRepository::min/max_block_height` + `ChainMeta::confirmation_depth`)
/// work unmodified for Tron — it just happens to be doing arithmetic on
/// timestamps instead of block counts for this one chain. The cost is
/// cosmetic: the `block` field on a Tron edge in `/graph` shows a ms
/// timestamp, not a literal block number.
#[derive(Clone)]
pub struct TronGridSource {
    base_url: String,
    api_key: Option<String>,
    client: reqwest::Client,
    page_cache: Cache<PageKey, PageValue>,
    file_cache_dir: Option<PathBuf>,
    max_pages_per_endpoint: u32,
    account_info_cache: Cache<Vec<u8>, Arc<dto::AccountInfo>>,
}

impl TronGridSource {
    pub fn new(client: reqwest::Client, config: TronGridConfig) -> Self {
        let page_cache = Cache::builder()
            .max_capacity(config.page_cache_max_capacity)
            .weigher(|_k: &PageKey, v: &PageValue| v.0.len().max(1) as u32)
            .time_to_live(config.page_cache_ttl)
            .build();
        let account_info_cache = Cache::builder()
            .max_capacity(50_000)
            .time_to_live(Duration::from_secs(24 * 60 * 60))
            .build();

        Self {
            base_url: config.base_url.trim_end_matches('/').to_string(),
            api_key: config.api_key,
            client,
            page_cache,
            file_cache_dir: config.file_cache_dir,
            max_pages_per_endpoint: config.max_pages_per_endpoint,
            account_info_cache,
        }
    }

    fn req(&self, url: &str) -> reqwest::RequestBuilder {
        let mut b = self.client.get(url);
        if let Some(key) = &self.api_key {
            b = b.header("TRON-PRO-API-KEY", key);
        }
        b
    }

    async fn http_get_text(&self, url: &str) -> DomainResult<String> {
        const MAX_ATTEMPTS: u8 = 3;
        let mut last_err = String::new();

        for attempt in 0..MAX_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(u64::from(attempt) * 2)).await;
            }
            tracing::debug!(url, attempt, "tronscan GET");

            let resp = match self.req(url).send().await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(url, attempt, error = %e, "request failed, retrying");
                    last_err = e.to_string();
                    continue;
                }
            };
            let status = resp.status();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                tracing::warn!(url, "tronscan rate limited");
                return Err(DomainError::InsufficientData("tronscan rate limited".into()));
            }
            let body = match resp.text().await {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(url, attempt, error = %e, "failed reading body");
                    last_err = e.to_string();
                    continue;
                }
            };
            if !status.is_success() {
                return Err(DomainError::InsufficientData(format!(
                    "tronscan http {status}: {body}"
                )));
            }
            return Ok(body);
        }
        Err(DomainError::InsufficientData(format!(
            "tronscan: after {MAX_ATTEMPTS} attempts: {last_err}"
        )))
    }

    fn file_path(
        &self,
        endpoint: &Endpoint,
        address_b58: &str,
        start: u32,
        min_ts: Option<u64>,
        max_ts: Option<u64>,
    ) -> Option<PathBuf> {
        let dir = self.file_cache_dir.as_ref()?;
        let min_s = min_ts.map(|v| v.to_string()).unwrap_or_else(|| "0".into());
        let max_s = max_ts.map(|v| v.to_string()).unwrap_or_else(|| "max".into());
        Some(dir.join(format!(
            "{}__{address_b58}__{min_s}__{max_s}__{start}.json",
            endpoint.prefix()
        )))
    }

    async fn body_for(
        &self,
        endpoint: &Endpoint,
        address_b58: &str,
        start: u32,
        min_ts: Option<u64>,
        max_ts: Option<u64>,
    ) -> DomainResult<String> {
        let path = self.file_path(endpoint, address_b58, start, min_ts, max_ts);
        if let Some(p) = path.as_deref()
            && let Ok(body) = tokio::fs::read_to_string(p).await
        {
            tracing::debug!(path = %p.display(), "tronscan file cache hit");
            return Ok(body);
        }
        let url = format!(
            "{}{}",
            self.base_url,
            match endpoint {
                Endpoint::Native =>
                    endpoints::transactions(address_b58, start, PAGE_SIZE, min_ts, max_ts),
                Endpoint::Trc20 =>
                    endpoints::trc20_transfers(address_b58, start, PAGE_SIZE, min_ts, max_ts),
            }
        );
        let body = self.http_get_text(&url).await?;
        if let Some(p) = path {
            if let Some(parent) = p.parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
            if let Err(e) = tokio::fs::write(&p, &body).await {
                tracing::warn!(path = %p.display(), error = %e, "tronscan cache write failed");
            }
        }
        Ok(body)
    }

    /// `min_ts`/`max_ts` are ms-since-epoch bounds forwarded to Tronscan's
    /// `start_timestamp`/`end_timestamp` query params, letting the API itself
    /// skip pages outside the requested window — this is what makes
    /// incremental (hot-tail / gap-only) refetch actually reduce network
    /// calls instead of always re-walking full history.
    async fn collect(
        &self,
        endpoint: Endpoint,
        address_b58: &str,
        max_transfers: usize,
        min_ts: Option<u64>,
        max_ts: Option<u64>,
    ) -> DomainResult<Vec<Transfer>> {
        let mut all = Vec::new();
        let mut start: u32 = 0;
        let mut page_n: u32 = 0;
        loop {
            let key = PageKey {
                endpoint,
                address_b58: address_b58.to_string(),
                start,
                min_ts,
                max_ts,
            };
            let value = if let Some(v) = self.page_cache.get(&key).await {
                v
            } else {
                let body = self
                    .body_for(&endpoint, address_b58, start, min_ts, max_ts)
                    .await?;
                let parsed = match endpoint {
                    Endpoint::Native => parse_native(&body)?,
                    Endpoint::Trc20 => parse_trc20(&body)?,
                };
                let arc = Arc::new(parsed);
                self.page_cache.insert(key, Arc::clone(&arc)).await;
                arc
            };
            page_n += 1;
            all.extend(value.0.iter().cloned());
            let is_last_page = value.1;
            if all.len() >= max_transfers || page_n >= self.max_pages_per_endpoint || is_last_page {
                break;
            }
            start += PAGE_SIZE;
        }
        tracing::debug!(
            address = address_b58,
            endpoint = endpoint.prefix(),
            pages = page_n,
            transfers = all.len(),
            ?min_ts,
            ?max_ts,
            "tronscan pagination done"
        );
        Ok(all)
    }

    /// Fetches and caches `/api/account` for `addr`. Tolerant of transport
    /// and parse failures (returns `Ok(None)`, mirroring how `is_contract`
    /// degrades elsewhere in this codebase) — only a hard rate-limit signal
    /// propagates as an `Err`, since that's the one condition callers should
    /// react to (backing off / failing over) rather than silently treating
    /// as "unknown".
    async fn account_info(&self, addr: &Address) -> DomainResult<Option<Arc<dto::AccountInfo>>> {
        let bytes = addr.bytes().to_vec();
        if let Some(cached) = self.account_info_cache.get(&bytes).await {
            return Ok(Some(cached));
        }

        let address_b58 = addr.canonical();
        let url = format!("{}{}", self.base_url, endpoints::account(&address_b58));
        let resp = match self.req(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "tronscan account request failed");
                return Ok(None);
            }
        };
        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(DomainError::RateLimited("tronscan account rate limited".into()));
        }
        if !resp.status().is_success() {
            tracing::debug!(status = %resp.status(), "tronscan account non-success status");
            return Ok(None);
        }
        let text = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "tronscan account body read failed");
                return Ok(None);
            }
        };
        let info: dto::AccountInfo = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, body = %text, "tronscan account parse failed");
                return Ok(None);
            }
        };

        let arc = Arc::new(info);
        self.account_info_cache.insert(bytes, Arc::clone(&arc)).await;
        Ok(Some(arc))
    }
}

#[async_trait]
impl ChainSource for TronGridSource {
    fn chain_id(&self) -> ChainId {
        ChainId::TRON
    }

    async fn latest_block(&self) -> DomainResult<BlockRef> {
        let url = format!("{}{}", self.base_url, endpoints::LATEST_BLOCK);
        let body = self.http_get_text(&url).await?;
        let latest: dto::LatestBlock = serde_json::from_str(&body)
            .map_err(|e| DomainError::InsufficientData(format!("block/latest parse: {e}")))?;

        tracing::debug!(number = latest.number(), "tronscan latest block");

        // `height` is the block's ms-since-epoch timestamp, not the literal
        // Tron block number — see the module doc comment above.
        let hash = parse_hash32_or_zero(latest.hash());
        Ok(BlockRef::new(ChainId::TRON, latest.timestamp().max(0) as u64, hash))
    }

    async fn fetch_block(&self, height: u64) -> DomainResult<NormalizedBlock> {
        Err(DomainError::InsufficientData(format!(
            "tronscan: fetch_block by height ({height}) not supported; use transfers_for_address"
        )))
    }

    async fn transfers_for_address(
        &self,
        addr: &Address,
        range: BlockRange,
        max_transfers: usize,
    ) -> DomainResult<Vec<Transfer>> {
        if addr.chain() != ChainId::TRON {
            return Err(DomainError::InsufficientData(format!(
                "tronscan source called with non-tron chain: {}",
                addr.chain()
            )));
        }
        let address_b58 = addr.canonical();

        // `range`'s bounds are ms-since-epoch timestamps for this source
        // (see module docs) — pass them straight through as Tronscan's
        // `start_timestamp`/`end_timestamp` filters, omitting the ones that
        // are just the unbounded default so a fresh (never-ingested)
        // address still gets a plain full-history fetch.
        let min_ts = (range.from_height() > 0).then_some(range.from_height());
        let max_ts = (range.to_height() < u64::MAX).then_some(range.to_height());

        tracing::info!(
            address = %address_b58,
            max_transfers,
            ?min_ts,
            ?max_ts,
            "fetching transfers from tronscan"
        );

        let (native, trc20) = tokio::try_join!(
            self.collect(Endpoint::Native, &address_b58, max_transfers, min_ts, max_ts),
            self.collect(Endpoint::Trc20, &address_b58, max_transfers, min_ts, max_ts),
        )?;

        tracing::info!(
            address = %address_b58,
            native = native.len(),
            trc20 = trc20.len(),
            "tronscan transfers fetched"
        );

        let mut out = native;
        out.extend(trc20);
        Ok(out)
    }

    async fn is_contract(&self, addr: &Address) -> DomainResult<Option<bool>> {
        if addr.chain() != ChainId::TRON {
            return Ok(None);
        }
        Ok(self.account_info(addr).await?.map(|info| info.is_contract()))
    }
}

/// Tronscan's own `/api/account` public tag (`addressTag`) doubles as this
/// source's `LabelProvider` — see the module doc comment on `TronGridSource`
/// for why `is_contract` and label resolution share the same cached fetch.
#[async_trait]
impl LabelProvider for TronGridSource {
    async fn resolve(&self, addr: &Address) -> DomainResult<Option<EntityLabel>> {
        if addr.chain() != ChainId::TRON {
            return Ok(None);
        }
        let Some(info) = self.account_info(addr).await? else {
            return Ok(None);
        };
        let Some(name) = info.address_tag() else {
            return Ok(None);
        };
        Ok(Some(EntityLabel::new(
            name.to_string(),
            info.address_tag_logo().map(str::to_owned),
            LabelSource::Community,
        )))
    }
}

fn parse_native(body: &str) -> DomainResult<(Vec<Transfer>, bool)> {
    let resp: dto::TransactionListResponse = serde_json::from_str(body).map_err(|e| {
        DomainError::InsufficientData(format!("tronscan native parse: {e}\n{body}"))
    })?;
    let rows = resp.into_data();
    let is_last_page = (rows.len() as u32) < PAGE_SIZE;
    let mut out = Vec::new();
    for raw in rows {
        match map_native(raw) {
            Ok(Some(t)) => out.push(t),
            Ok(None) => {}
            Err(e) => tracing::debug!(error = %e, "skip non-transfer native tx"),
        }
    }
    Ok((out, is_last_page))
}

fn parse_trc20(body: &str) -> DomainResult<(Vec<Transfer>, bool)> {
    let resp: dto::Trc20TransferListResponse = serde_json::from_str(body).map_err(|e| {
        DomainError::InsufficientData(format!("tronscan trc20 parse: {e}\n{body}"))
    })?;
    let rows = resp.into_transfers();
    let is_last_page = (rows.len() as u32) < PAGE_SIZE;
    let mut out = Vec::new();
    for rec in rows {
        match map_trc20(rec) {
            Ok(Some(t)) => out.push(t),
            Ok(None) => {}
            Err(e) => tracing::debug!(error = %e, "skip malformed trc20 row"),
        }
    }
    Ok((out, is_last_page))
}

fn map_native(raw: dto::RawTransaction) -> anyhow::Result<Option<Transfer>> {
    // Only `TransferContract` rows move native TRX value; everything else
    // (contract triggers, resource delegation, votes, ...) is out of scope
    // here — TRC20 value moves are covered separately by `map_trc20`.
    if raw.contract_type() != dto::TRANSFER_CONTRACT_TYPE {
        return Ok(None);
    }

    let tx_hash = parse_hash32(raw.hash()).context("tron tx hash")?;
    let block_ts = raw.timestamp();
    let timestamp = chrono::Utc
        .timestamp_millis_opt(block_ts)
        .single()
        .ok_or_else(|| anyhow!("bad timestamp {}", block_ts))?;

    let finality = match raw.contract_ret() {
        Some("SUCCESS") => Finality::Confirmed,
        Some(_) => Finality::Reorged,
        None => Finality::Confirmed,
    };

    let Some(to_address) = raw.to_address() else {
        return Ok(None);
    };
    let from = Address::parse(ChainId::TRON, raw.owner_address()).context("from")?;
    let to = Address::parse(ChainId::TRON, to_address).context("to")?;

    let amount: u128 = raw.amount().unwrap_or("0").parse().context("amount")?;
    if amount == 0 {
        return Ok(None);
    }

    // `height` = block_timestamp (ms since epoch), not the real Tron block
    // number — this source's incremental-fetch convention (see module docs).
    let block_ref = BlockRef::new(ChainId::TRON, block_ts.max(0) as u64, tx_hash);
    Ok(Some(Transfer::new(
        TransferId::new(ChainId::TRON, tx_hash, 0),
        ChainId::TRON,
        TxRef::new(ChainId::TRON, tx_hash),
        from,
        to,
        AssetId::native(ChainId::TRON),
        Amount::new(U256::from(amount), TRX_DECIMALS),
        block_ref,
        timestamp,
        TransferKind::Native,
        finality,
    )))
}

fn map_trc20(rec: dto::Trc20Transfer) -> anyhow::Result<Option<Transfer>> {
    let tx_hash = parse_hash32(rec.transaction_id()).context("trc20 tx hash")?;
    let block_ts = rec.block_ts();
    let timestamp = chrono::Utc
        .timestamp_millis_opt(block_ts)
        .single()
        .ok_or_else(|| anyhow!("bad trc20 timestamp {}", block_ts))?;

    // Tronscan doesn't resolve `tokenInfo` (and therefore decimals) for
    // every contract it lists transfers for — see `Trc20TokenInfo` doc
    // comment. Without decimals we can't normalize `quant` into an `Amount`,
    // so skip rather than guess.
    let Some(decimals) = rec.token_info().token_decimal() else {
        return Ok(None);
    };

    let from = Address::parse(ChainId::TRON, rec.from_address())
        .map_err(|e| anyhow!("trc20 from: {e}"))?;
    let to = Address::parse(ChainId::TRON, rec.to_address())
        .map_err(|e| anyhow!("trc20 to: {e}"))?;
    let contract = Address::parse(ChainId::TRON, rec.contract_address())
        .map_err(|e| anyhow!("trc20 contract: {e}"))?;
    let raw = U256::from_dec_str(rec.quant()).context("trc20 value")?;
    let symbol = rec.token_info().token_abbr().filter(|s| !s.is_empty()).map(str::to_owned);

    // `height` = block_timestamp (ms since epoch); see module docs. The
    // TRC20 endpoint doesn't expose a real block number at all, so this is
    // also the only option here, not just a convention shared with native.
    let block_ref = BlockRef::new(ChainId::TRON, block_ts.max(0) as u64, tx_hash);
    Ok(Some(Transfer::new(
        TransferId::new(ChainId::TRON, tx_hash, 0),
        ChainId::TRON,
        TxRef::new(ChainId::TRON, tx_hash),
        from,
        to,
        AssetId::contract(ChainId::TRON, contract.bytes().to_vec()),
        Amount::new(raw, decimals),
        block_ref,
        timestamp,
        TransferKind::Token {
            contract,
            standard: TokenStandard::Trc20,
            symbol,
        },
        Finality::Confirmed,
    )))
}

fn parse_hash32(s: &str) -> anyhow::Result<[u8; 32]> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(s).context("hex decode")?;
    bytes
        .try_into()
        .map_err(|v: Vec<u8>| anyhow!("expected 32 bytes, got {}", v.len()))
}

fn parse_hash32_or_zero(s: &str) -> [u8; 32] {
    parse_hash32(s).unwrap_or([0u8; 32])
}

/// Fixtures below are trimmed real responses captured from
/// `apilist.tronscanapi.com` — they pin this module to the API's actual
/// shape rather than a guessed one.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_native_filters_non_transfer_contract_types_and_maps_transfer_contract() {
        let body = r#"{
            "total": 2,
            "data": [
                {
                    "block": 83928786,
                    "hash": "ae35b9769b87c5ae779a61883bacaff127046920783e620e306be26375e94748",
                    "timestamp": 1782455967000,
                    "ownerAddress": "TSpaZNL6mbKTQAyRVo5fXUTfPi6V7t8HAF",
                    "toAddress": "TWd4WrZ9wn84f5x1hZhL4DHvk738ns5jwb",
                    "contractType": 1,
                    "confirmed": true,
                    "contractRet": "SUCCESS",
                    "amount": "7"
                },
                {
                    "block": 84421576,
                    "hash": "not-a-real-hash-but-never-parsed-since-contractType-is-filtered",
                    "timestamp": 1783934784000,
                    "ownerAddress": "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t",
                    "toAddress": "TF5r1EqCCrUa76MmCmEaxLvcADVHGSzAit",
                    "contractType": 31,
                    "confirmed": true,
                    "contractRet": "SUCCESS",
                    "amount": "0"
                }
            ]
        }"#;

        let (transfers, is_last_page) = parse_native(body).expect("parse ok");
        assert!(is_last_page, "2 rows < PAGE_SIZE must signal last page");
        assert_eq!(transfers.len(), 1, "only the TransferContract row should map");

        let t = &transfers[0];
        assert_eq!(t.from().canonical(), "TSpaZNL6mbKTQAyRVo5fXUTfPi6V7t8HAF");
        assert_eq!(t.to().canonical(), "TWd4WrZ9wn84f5x1hZhL4DHvk738ns5jwb");
        assert_eq!(t.amount().raw(), U256::from(7u64));
        assert_eq!(t.amount().decimals(), TRX_DECIMALS);
        assert_eq!(t.block().height(), 1782455967000);
        assert_eq!(t.finality(), Finality::Confirmed);
    }

    #[test]
    fn parse_native_skips_zero_amount_rows() {
        let body = r#"{
            "total": 1,
            "data": [{
                "block": 1,
                "hash": "ae35b9769b87c5ae779a61883bacaff127046920783e620e306be26375e94748",
                "timestamp": 1782455967000,
                "ownerAddress": "TSpaZNL6mbKTQAyRVo5fXUTfPi6V7t8HAF",
                "toAddress": "TWd4WrZ9wn84f5x1hZhL4DHvk738ns5jwb",
                "contractType": 1,
                "confirmed": true,
                "contractRet": "SUCCESS",
                "amount": "0"
            }]
        }"#;
        let (transfers, _) = parse_native(body).expect("parse ok");
        assert!(transfers.is_empty());
    }

    #[test]
    fn parse_trc20_maps_resolved_tokens_and_skips_rows_missing_decimals() {
        let body = r#"{
            "total": 2,
            "token_transfers": [
                {
                    "transaction_id": "a044bddfa3be40af87f2011c74f0982fdd307bf0ba5c0966b6364368068090f9",
                    "block": 84407622,
                    "block_ts": 1783892910000,
                    "from_address": "TBedwuD4zRp6DsVJhKC9YSgVYCco6JtGBm",
                    "to_address": "TWd4WrZ9wn84f5x1hZhL4DHvk738ns5jwb",
                    "contract_address": "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t",
                    "quant": "1500000",
                    "confirmed": true,
                    "contractRet": "SUCCESS",
                    "tokenInfo": { "tokenAbbr": "USDT", "tokenDecimal": 6 }
                },
                {
                    "transaction_id": "0a80746bbee0fd9dc0e3378d4f32cf099320fda76446f423bdf433756935a30a",
                    "block": 84421576,
                    "block_ts": 1783934772000,
                    "from_address": "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t",
                    "to_address": "THz4Tnsbt6nV1q4SdBodcgPfk1NbK5hEAE",
                    "contract_address": "TYBWsdgrf6JfvzNiTnazDDJrQCEhyfWZK8",
                    "quant": "800000000",
                    "confirmed": true,
                    "contractRet": "SUCCESS",
                    "tokenInfo": {}
                }
            ]
        }"#;

        let (transfers, is_last_page) = parse_trc20(body).expect("parse ok");
        assert!(is_last_page);
        assert_eq!(
            transfers.len(),
            1,
            "the row with empty tokenInfo (unresolved decimals) must be skipped, not guessed"
        );

        let t = &transfers[0];
        assert_eq!(t.amount().raw(), U256::from(1_500_000u64));
        assert_eq!(t.amount().decimals(), 6);
        match t.kind() {
            TransferKind::Token { symbol, .. } => assert_eq!(symbol.as_deref(), Some("USDT")),
            other => panic!("expected Token kind, got {other:?}"),
        }
    }

    #[test]
    fn account_info_exposes_tag_only_when_present_and_nonblank() {
        let tagged: dto::AccountInfo = serde_json::from_str(
            r#"{"accountType":0,"addressTag":"Binance-Cold 2","addressTagLogo":"https://coin.top/production/upload/logo/Binance.png"}"#,
        )
        .unwrap();
        assert!(!tagged.is_contract());
        assert_eq!(tagged.address_tag(), Some("Binance-Cold 2"));
        assert_eq!(
            tagged.address_tag_logo(),
            Some("https://coin.top/production/upload/logo/Binance.png")
        );

        let untagged: dto::AccountInfo =
            serde_json::from_str(r#"{"accountType":2,"addressTagLogo":""}"#).unwrap();
        assert!(untagged.is_contract());
        assert_eq!(untagged.address_tag(), None);
        assert_eq!(untagged.address_tag_logo(), None);
    }

    #[test]
    fn latest_block_parses_number_hash_and_timestamp() {
        let body = r#"{
            "hash":"0000000005082de78c9fc7814e564675ad50bf86f8956ab35bdc62b4c3665f41",
            "confirmed":true,
            "number":84422119,
            "timestamp":1783936413000
        }"#;
        let latest: dto::LatestBlock = serde_json::from_str(body).unwrap();
        assert_eq!(latest.number(), 84422119);
        assert_eq!(latest.timestamp(), 1783936413000);
        assert_eq!(parse_hash32(latest.hash()).unwrap().len(), 32);
    }
}
