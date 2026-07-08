use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, anyhow};
use async_trait::async_trait;
use chrono::TimeZone;
use moka::future::Cache;

use domain::{
    asset::{AssetId, TokenStandard},
    chain::ChainId,
    error::{DomainError, DomainResult},
    ports::{BlockRange, ChainSource},
    primitives::{Address, Amount, BlockRef, TxRef, U256},
    transfer::{Finality, NormalizedBlock, Transfer, TransferId, TransferKind},
};

use crate::fetch_wallet_api::side_api::tron::{dto, endpoints};

const TRX_DECIMALS: u8 = 6;
const TRON_VERSION_BYTE: u8 = 0x41;

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

    /// Per-chain endpoint override (Tron mainnet vs Shasta testnet vs
    /// self-hosted node). The default is `api.trongrid.io`.
    pub fn with_base_url(mut self, url: String) -> Self {
        self.base_url = url;
        self
    }
}

impl Default for TronGridConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.trongrid.io".into(),
            api_key: None,
            page_cache_max_capacity: 10_000,
            page_cache_ttl: Duration::from_secs(60 * 60),
            file_cache_dir: None,
            max_pages_per_endpoint: 50,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
    fingerprint: Option<String>,
}

type PageValue = Arc<(Vec<Transfer>, Option<String>)>;

/// Reads only solidified (`only_confirmed=true`) data — see
/// `side_api/tron/endpoints.rs`. TRON DPoS finality means solidified blocks
/// never reorg, so the hot-tail problem (relevant for ETH/Moralis) doesn't
/// apply here and pages can be cached aggressively with a single TTL.
#[derive(Clone)]
pub struct TronGridSource {
    base_url: String,
    api_key: Option<String>,
    client: reqwest::Client,
    page_cache: Cache<PageKey, PageValue>,
    file_cache_dir: Option<PathBuf>,
    max_pages_per_endpoint: u32,
}

impl TronGridSource {
    pub fn new(client: reqwest::Client, config: TronGridConfig) -> Self {
        let page_cache = Cache::builder()
            .max_capacity(config.page_cache_max_capacity)
            .weigher(|_k: &PageKey, v: &PageValue| v.0.len().max(1) as u32)
            .time_to_live(config.page_cache_ttl)
            .build();

        Self {
            base_url: config.base_url.trim_end_matches('/').to_string(),
            api_key: config.api_key,
            client,
            page_cache,
            file_cache_dir: config.file_cache_dir,
            max_pages_per_endpoint: config.max_pages_per_endpoint,
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
            tracing::debug!(url, attempt, "trongrid GET");

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
                tracing::warn!(url, "trongrid rate limited");
                return Err(DomainError::InsufficientData("trongrid rate limited".into()));
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
                    "trongrid http {status}: {body}"
                )));
            }
            return Ok(body);
        }
        Err(DomainError::InsufficientData(format!(
            "trongrid: after {MAX_ATTEMPTS} attempts: {last_err}"
        )))
    }

    fn file_path(
        &self,
        endpoint: &Endpoint,
        address_b58: &str,
        fingerprint: Option<&str>,
    ) -> Option<PathBuf> {
        let dir = self.file_cache_dir.as_ref()?;
        let fp = match fingerprint {
            None => "nil".to_string(),
            Some(f) => {
                use sha2::{Digest, Sha256};
                hex::encode(Sha256::digest(f.as_bytes()))
            }
        };
        Some(dir.join(format!("{}__{address_b58}__{fp}.json", endpoint.prefix())))
    }

    async fn body_for(
        &self,
        endpoint: &Endpoint,
        address_b58: &str,
        fingerprint: Option<&str>,
    ) -> DomainResult<String> {
        let path = self.file_path(endpoint, address_b58, fingerprint);
        if let Some(p) = path.as_deref()
            && let Ok(body) = tokio::fs::read_to_string(p).await
        {
            tracing::debug!(path = %p.display(), "trongrid file cache hit");
            return Ok(body);
        }
        let url = format!(
            "{}{}",
            self.base_url,
            match endpoint {
                Endpoint::Native => endpoints::native_transfers(address_b58, fingerprint),
                Endpoint::Trc20 => endpoints::trc20_transfers(address_b58, fingerprint),
            }
        );
        let body = self.http_get_text(&url).await?;
        if let Some(p) = path {
            if let Some(parent) = p.parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
            if let Err(e) = tokio::fs::write(&p, &body).await {
                tracing::warn!(path = %p.display(), error = %e, "trongrid cache write failed");
            }
        }
        Ok(body)
    }

    async fn collect(
        &self,
        endpoint: Endpoint,
        address_b58: &str,
        max_transfers: usize,
    ) -> DomainResult<Vec<Transfer>> {
        let mut all = Vec::new();
        let mut fingerprint: Option<String> = None;
        let mut page_n: u32 = 0;
        loop {
            let key = PageKey {
                endpoint: endpoint.clone(),
                address_b58: address_b58.to_string(),
                fingerprint: fingerprint.clone(),
            };
            let value = if let Some(v) = self.page_cache.get(&key).await {
                v
            } else {
                let body = self
                    .body_for(&endpoint, address_b58, fingerprint.as_deref())
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
            if all.len() >= max_transfers || page_n >= self.max_pages_per_endpoint {
                break;
            }
            match value.1.clone() {
                Some(fp) => fingerprint = Some(fp),
                None => break,
            }
        }
        tracing::debug!(
            address = address_b58,
            endpoint = endpoint.prefix(),
            pages = page_n,
            transfers = all.len(),
            "trongrid pagination done"
        );
        Ok(all)
    }
}

#[async_trait]
impl ChainSource for TronGridSource {
    fn chain_id(&self) -> ChainId {
        ChainId::TRON
    }

    async fn latest_block(&self) -> DomainResult<BlockRef> {
        let url = format!("{}/wallet/getnowblock", self.base_url);
        let body = self.http_get_text(&url).await?;
        let value: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| DomainError::InsufficientData(format!("getnowblock parse: {e}")))?;

        let height = value
            .get("block_header")
            .and_then(|h| h.get("raw_data"))
            .and_then(|r| r.get("number"))
            .and_then(|n| n.as_u64())
            .unwrap_or(0);
        let hash_hex = value
            .get("blockID")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let hash = parse_hash32_or_zero(hash_hex);
        Ok(BlockRef::new(ChainId::TRON, height, hash))
    }

    async fn fetch_block(&self, height: u64) -> DomainResult<NormalizedBlock> {
        Err(DomainError::InsufficientData(format!(
            "trongrid: fetch_block by height ({height}) not supported; use transfers_for_address"
        )))
    }

    async fn transfers_for_address(
        &self,
        addr: &Address,
        _range: BlockRange,
        max_transfers: usize,
    ) -> DomainResult<Vec<Transfer>> {
        if addr.chain() != ChainId::TRON {
            return Err(DomainError::InsufficientData(format!(
                "trongrid source called with non-tron chain: {}",
                addr.chain()
            )));
        }
        let address_b58 = addr.canonical();
        tracing::info!(address = %address_b58, max_transfers, "fetching transfers from trongrid");

        let (native, trc20) = tokio::try_join!(
            self.collect(Endpoint::Native, &address_b58, max_transfers),
            self.collect(Endpoint::Trc20, &address_b58, max_transfers),
        )?;

        tracing::info!(
            address = %address_b58,
            native = native.len(),
            trc20 = trc20.len(),
            "trongrid transfers fetched"
        );

        let mut out = native;
        out.extend(trc20);
        Ok(out)
    }
}

fn parse_native(body: &str) -> DomainResult<(Vec<Transfer>, Option<String>)> {
    let resp: dto::NativeTxResponse = serde_json::from_str(body).map_err(|e| {
        DomainError::InsufficientData(format!("trongrid native parse: {e}\n{body}"))
    })?;
    let (data, meta) = resp.into_parts();
    let fingerprint = meta.and_then(|m| m.fingerprint());
    let mut out = Vec::new();
    for raw in data {
        match map_native(raw) {
            Ok(Some(t)) => out.push(t),
            Ok(None) => {}
            Err(e) => tracing::debug!(error = %e, "skip non-transfer native tx"),
        }
    }
    Ok((out, fingerprint))
}

fn parse_trc20(body: &str) -> DomainResult<(Vec<Transfer>, Option<String>)> {
    let resp: dto::Trc20Response = serde_json::from_str(body).map_err(|e| {
        DomainError::InsufficientData(format!("trongrid trc20 parse: {e}\n{body}"))
    })?;
    let (data, meta) = resp.into_parts();
    let fingerprint = meta.and_then(|m| m.fingerprint());
    let mut out = Vec::new();
    for rec in data {
        match map_trc20(rec) {
            Ok(t) => out.push(t),
            Err(e) => tracing::debug!(error = %e, "skip malformed trc20 row"),
        }
    }
    Ok((out, fingerprint))
}

fn map_native(raw: dto::RawTransaction) -> anyhow::Result<Option<Transfer>> {
    let tx_hash = parse_hash32(raw.tx_id()).context("tron tx hash")?;
    let block_ts = raw.block_timestamp();
    let timestamp = chrono::Utc
        .timestamp_millis_opt(block_ts)
        .single()
        .ok_or_else(|| anyhow!("bad timestamp {}", block_ts))?;

    let finality = match raw.ret().and_then(|r| r.first()).and_then(|r| r.contract_ret()) {
        Some("SUCCESS") => Finality::Confirmed,
        Some(_) => Finality::Reorged,
        None => Finality::Confirmed,
    };

    let Some(contract) = raw.into_raw_data().into_contracts().into_iter().next() else {
        return Ok(None);
    };
    if contract.contract_type() != "TransferContract" {
        return Ok(None);
    }
    let v: dto::TransferContractValue = serde_json::from_value(contract.into_parameter().into_value())
        .context("decode TransferContract value")?;

    let from = parse_tron_hex_address(v.owner_address()).context("from")?;
    let to = parse_tron_hex_address(v.to_address()).context("to")?;
    if v.amount() == 0 {
        return Ok(None);
    }

    let block_ref = BlockRef::new(ChainId::TRON, 0, tx_hash);
    Ok(Some(Transfer::new(
        TransferId::new(ChainId::TRON, tx_hash, 0),
        ChainId::TRON,
        TxRef::new(ChainId::TRON, tx_hash),
        from,
        to,
        AssetId::native(ChainId::TRON),
        Amount::new(U256::from(v.amount()), TRX_DECIMALS),
        block_ref,
        timestamp,
        TransferKind::Native,
        finality,
    )))
}

fn map_trc20(rec: dto::Trc20Transfer) -> anyhow::Result<Transfer> {
    let tx_hash = parse_hash32(rec.transaction_id()).context("trc20 tx hash")?;
    let block_ts = rec.block_timestamp();
    let timestamp = chrono::Utc
        .timestamp_millis_opt(block_ts)
        .single()
        .ok_or_else(|| anyhow!("bad trc20 timestamp {}", block_ts))?;

    let from = Address::parse(ChainId::TRON, rec.from())
        .map_err(|e| anyhow!("trc20 from: {e}"))?;
    let to = Address::parse(ChainId::TRON, rec.to())
        .map_err(|e| anyhow!("trc20 to: {e}"))?;
    let contract = Address::parse(ChainId::TRON, rec.token_info().address())
        .map_err(|e| anyhow!("trc20 contract: {e}"))?;
    let raw = U256::from_dec_str(rec.value()).context("trc20 value")?;
    let decimals = rec.token_info().decimals();
    let symbol = {
        let s = rec.token_info().symbol();
        if s.is_empty() { None } else { Some(s.to_string()) }
    };

    let block_ref = BlockRef::new(ChainId::TRON, 0, tx_hash);
    Ok(Transfer::new(
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
    ))
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

fn parse_tron_hex_address(s: &str) -> anyhow::Result<Address> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(s).context("hex decode tron addr")?;
    if bytes.len() != 21 {
        return Err(anyhow!("tron hex address expected 21 bytes, got {}", bytes.len()));
    }
    if bytes[0] != TRON_VERSION_BYTE {
        return Err(anyhow!("tron version byte expected 0x41, got 0x{:02x}", bytes[0]));
    }
    Ok(Address::new(ChainId::TRON, bytes))
}
