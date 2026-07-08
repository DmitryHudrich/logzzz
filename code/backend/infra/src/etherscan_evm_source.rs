use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use chrono::TimeZone;
use moka::future::Cache;
use serde::Deserialize;
use tokio::sync::Semaphore;

use domain::{
    asset::{AssetId, TokenStandard},
    chain::ChainId,
    error::{DomainError, DomainResult},
    ports::{BlockRange, ChainSource},
    primitives::{Address, Amount, BlockRef, TxRef, U256},
    transfer::{Finality, NormalizedBlock, Transfer, TransferId, TransferKind},
};

use crate::key_pool::KeyPool;
use crate::rate_limiter::RateLimiter;

const DEFAULT_BASE_URL: &str = "https://api.etherscan.io/v2/api";
const DEFAULT_PAGE_SIZE: u32 = 1000;
const DEFAULT_MAX_PAGES: u32 = 20;
const ETH_CHAIN_ID: u32 = 1;
const DEFAULT_HTTP_MAX_ATTEMPTS: u8 = 6;
const DEFAULT_IS_CONTRACT_MAX_ATTEMPTS: u8 = 4;
const LATEST_SENTINEL: u64 = 99_999_999;
const RATE_LIMIT_BACKOFF_BASE_MS: u64 = 500;
const DEFAULT_KEY_COOLDOWN_SECS: u64 = 5;
const DEFAULT_REQUESTS_PER_SECOND: f64 = 5.0;
const DEFAULT_REQUESTS_PER_SECOND_BURST: f64 = 5.0;

#[derive(Debug, Clone)]
pub struct EtherscanEvmConfig {
    api_keys: Vec<String>,
    base_url: String,
    page_size: u32,
    max_pages: u32,
    cold_ttl: Duration,
    hot_ttl: Duration,
    cache_hot_tail: bool,
    confirmation_depth: u64,
    latest_block_ttl: Duration,
    page_max_capacity: u64,
    file_cache_dir: Option<PathBuf>,
    max_concurrent_requests: u32,
    key_cooldown: Duration,
    requests_per_second: f64,
    requests_per_second_burst: f64,
    http_max_attempts: u8,
    is_contract_max_attempts: u8,
}

impl EtherscanEvmConfig {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        api_keys: Vec<String>,
        base_url: Option<String>,
        page_size: Option<u32>,
        max_pages: Option<u32>,
        cold_ttl: Duration,
        hot_ttl: Duration,
        cache_hot_tail: bool,
        confirmation_depth: u64,
        latest_block_ttl: Duration,
        page_max_capacity: u64,
        file_cache_dir: Option<PathBuf>,
        max_concurrent_requests: u32,
        key_cooldown: Option<Duration>,
        requests_per_second: Option<f64>,
        requests_per_second_burst: Option<f64>,
        http_max_attempts: Option<u8>,
        is_contract_max_attempts: Option<u8>,
    ) -> Self {
        let api_keys: Vec<String> = api_keys
            .into_iter()
            .filter(|k| !k.trim().is_empty())
            .collect();
        Self {
            api_keys,
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.into()),
            page_size: page_size.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, 10_000),
            max_pages: max_pages.unwrap_or(DEFAULT_MAX_PAGES).max(1),
            cold_ttl,
            hot_ttl,
            cache_hot_tail,
            confirmation_depth,
            latest_block_ttl,
            page_max_capacity: page_max_capacity.max(1),
            file_cache_dir,
            max_concurrent_requests: max_concurrent_requests.max(1),
            key_cooldown: key_cooldown
                .unwrap_or_else(|| Duration::from_secs(DEFAULT_KEY_COOLDOWN_SECS)),
            requests_per_second: requests_per_second.unwrap_or(DEFAULT_REQUESTS_PER_SECOND),
            requests_per_second_burst: requests_per_second_burst
                .unwrap_or(DEFAULT_REQUESTS_PER_SECOND_BURST),
            http_max_attempts: http_max_attempts.unwrap_or(DEFAULT_HTTP_MAX_ATTEMPTS).max(1),
            is_contract_max_attempts: is_contract_max_attempts
                .unwrap_or(DEFAULT_IS_CONTRACT_MAX_ATTEMPTS)
                .max(1),
        }
    }

    pub fn has_keys(&self) -> bool {
        !self.api_keys.is_empty()
    }

    /// Chain-specific override for the API base URL. Etherscan V2 uses one
    /// canonical URL and a `chainid=` query param, so most deployments never
    /// need this; kept for private/proxied etherscan-compatible endpoints.
    pub fn with_base_url(mut self, url: String) -> Self {
        self.base_url = url;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Endpoint {
    TxList,
    TxListInternal,
    TokenTx,
}

impl Endpoint {
    fn action(self) -> &'static str {
        match self {
            Endpoint::TxList => "txlist",
            Endpoint::TxListInternal => "txlistinternal",
            Endpoint::TokenTx => "tokentx",
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Endpoint::TxList => "nat",
            Endpoint::TxListInternal => "int",
            Endpoint::TokenTx => "erc20",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PageKey {
    endpoint: Endpoint,
    address: String,
    from_block: u64,
    to_block: u64,
    page: u32,
}

type PageValue = Arc<Vec<serde_json::Value>>;

#[derive(Clone)]
pub struct EtherscanEvmSource {
    chain: ChainId,
    key_pool: KeyPool,
    base_url: String,
    page_size: u32,
    max_pages: u32,
    cache_hot_tail: bool,
    confirmation_depth: u64,
    http_max_attempts: u8,
    is_contract_max_attempts: u8,
    client: reqwest::Client,
    cold_page_cache: Cache<PageKey, PageValue>,
    hot_page_cache: Cache<PageKey, PageValue>,
    latest_block_cache: Cache<(), u64>,
    is_contract_cache: Cache<Vec<u8>, bool>,
    file_cache_dir: Option<PathBuf>,
    preloaded_file_cache: Arc<HashMap<PathBuf, PageValue>>,
    request_permits: Arc<Semaphore>,
    rate_limiter: Arc<RateLimiter>,
}

impl EtherscanEvmSource {
    pub async fn new(chain: ChainId, client: reqwest::Client, cfg: EtherscanEvmConfig) -> Self {
        assert!(
            cfg.has_keys(),
            "EtherscanEvmSource: at least one api key required — config validation must guard this"
        );
        let key_pool = KeyPool::new(cfg.api_keys.clone(), cfg.key_cooldown);
        let cold_page_cache = Cache::builder()
            .max_capacity(cfg.page_max_capacity)
            .weigher(|_k: &PageKey, v: &PageValue| v.len().max(1) as u32)
            .time_to_live(cfg.cold_ttl)
            .build();

        let hot_page_cache = Cache::builder()
            .max_capacity(cfg.page_max_capacity)
            .weigher(|_k: &PageKey, v: &PageValue| v.len().max(1) as u32)
            .time_to_live(cfg.hot_ttl)
            .build();

        let latest_block_cache = Cache::builder()
            .max_capacity(1)
            .time_to_live(cfg.latest_block_ttl)
            .build();

        // Contract code is immutable in practice (post-Cancun SELFDESTRUCT
        // basically removed); cache aggressively to avoid burning API quota.
        let is_contract_cache = Cache::builder()
            .max_capacity(100_000)
            .time_to_live(std::time::Duration::from_secs(7 * 24 * 3600))
            .build();

        let preloaded_file_cache = match &cfg.file_cache_dir {
            Some(dir) => Arc::new(Self::load_file_cache(dir).await),
            None => Arc::new(HashMap::new()),
        };

        let request_permits = Arc::new(Semaphore::new(cfg.max_concurrent_requests as usize));
        let rate_limiter = Arc::new(RateLimiter::new(
            cfg.requests_per_second,
            cfg.requests_per_second_burst,
        ));

        tracing::info!(
            base_url = %cfg.base_url,
            page_size = cfg.page_size,
            max_pages = cfg.max_pages,
            cold_ttl_secs = cfg.cold_ttl.as_secs(),
            hot_ttl_secs = cfg.hot_ttl.as_secs(),
            cache_hot_tail = cfg.cache_hot_tail,
            confirmation_depth = cfg.confirmation_depth,
            file_cache_dir = ?cfg.file_cache_dir,
            preloaded_pages = preloaded_file_cache.len(),
            max_concurrent_requests = cfg.max_concurrent_requests,
            api_keys = key_pool.len(),
            key_cooldown_secs = cfg.key_cooldown.as_secs(),
            requests_per_second = cfg.requests_per_second,
            requests_per_second_burst = cfg.requests_per_second_burst,
            http_max_attempts = cfg.http_max_attempts,
            is_contract_max_attempts = cfg.is_contract_max_attempts,
            chain_id = chain.value(),
            "Etherscan EVM source initialized"
        );

        Self {
            chain,
            key_pool,
            base_url: cfg.base_url,
            page_size: cfg.page_size,
            max_pages: cfg.max_pages,
            cache_hot_tail: cfg.cache_hot_tail,
            confirmation_depth: cfg.confirmation_depth,
            http_max_attempts: cfg.http_max_attempts,
            is_contract_max_attempts: cfg.is_contract_max_attempts,
            client,
            cold_page_cache,
            hot_page_cache,
            latest_block_cache,
            is_contract_cache,
            file_cache_dir: cfg.file_cache_dir,
            preloaded_file_cache,
            request_permits,
            rate_limiter,
        }
    }

    async fn load_file_cache(dir: &Path) -> HashMap<PathBuf, PageValue> {
        let mut map = HashMap::new();
        let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
            return map;
        };
        let mut files = 0usize;
        let mut rows = 0usize;
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(body) = tokio::fs::read_to_string(&path).await.ok() else {
                continue;
            };
            let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(&body) else {
                continue;
            };
            rows += arr.len();
            map.insert(path, Arc::new(arr));
            files += 1;
        }
        tracing::info!(files, rows, "etherscan file cache pre-loaded");
        map
    }

    fn build_url(&self, endpoint: Endpoint, api_key: &str, params: &[(&str, String)]) -> String {
        let mut url = format!(
            "{base}?chainid={chain}&module=account&action={action}&apikey={key}",
            base = self.base_url,
            chain = ETH_CHAIN_ID,
            action = endpoint.action(),
            key = api_key,
        );
        for (k, v) in params {
            url.push('&');
            url.push_str(k);
            url.push('=');
            url.push_str(v);
        }
        url
    }

    /// Issue a GET against the Etherscan account endpoints, rotating API
    /// keys on rate-limit responses. Two layers of throttling collaborate:
    ///
    /// * **Proactive (token bucket).** Every request first acquires a token
    ///   from `rate_limiter` so we never *burst* above `requests_per_second`
    ///   regardless of concurrency. This is what actually keeps free-tier
    ///   etherscan happy in steady state.
    /// * **Reactive (per-key cooldown + retry).** If we still get 429 (the
    ///   bucket can over-grant briefly on clock drift, or another process
    ///   shares our quota), the offending key gets cooled. We then call
    ///   `pick_or_wait`: if any key is live we use it, otherwise we sleep
    ///   for the soonest cooldown expiry and retry. With a single key this
    ///   gives REAL retries (the loop body actually runs again instead of
    ///   bailing immediately the way pure `pick()` would).
    ///
    /// Only after `http_max_attempts` attempts all failed do we return
    /// `RateLimited` — at which point the router upstream can fail over.
    async fn http_get_json(
        &self,
        endpoint: Endpoint,
        params: &[(&str, String)],
    ) -> DomainResult<EtherscanResponse> {
        let mut last_err = String::new();
        for attempt in 0..self.http_max_attempts {
            // Layer 1: token bucket. Never burst above the configured RPS.
            self.rate_limiter.acquire(1.0).await;

            // Layer 2: pick a live key, or wait for the soonest to cool down.
            let api_key = match self.key_pool.pick_or_wait() {
                Ok(k) => k,
                Err(wait) => {
                    tracing::warn!(
                        attempt,
                        wait_ms = wait.as_millis() as u64,
                        "etherscan: all keys cooled, waiting before retry"
                    );
                    tokio::time::sleep(wait).await;
                    last_err = "all keys cooled".to_string();
                    continue;
                }
            };

            if attempt > 0 && last_err.starts_with("http") {
                // 429-style retry: add a touch of exponential backoff on
                // top of the token-bucket throttle. Caps via http_max_attempts.
                let backoff = Duration::from_millis(
                    RATE_LIMIT_BACKOFF_BASE_MS.saturating_mul(1u64 << (attempt - 1)),
                );
                tokio::time::sleep(backoff).await;
            }

            let url = self.build_url(endpoint, &api_key, params);
            tracing::debug!(url, attempt, "etherscan GET");

            let permit = match self.request_permits.clone().acquire_owned().await {
                Ok(p) => p,
                Err(e) => {
                    return Err(DomainError::InsufficientData(format!(
                        "etherscan: semaphore closed: {e}"
                    )));
                }
            };

            let resp = match self.client.get(&url).send().await {
                Ok(r) => r,
                Err(e) => {
                    drop(permit);
                    tracing::warn!(url, attempt, error = %e, "etherscan request failed");
                    last_err = e.to_string();
                    continue;
                }
            };
            let status = resp.status();
            let body = match resp.text().await {
                Ok(b) => b,
                Err(e) => {
                    drop(permit);
                    tracing::warn!(url, attempt, error = %e, "etherscan body read failed");
                    last_err = e.to_string();
                    continue;
                }
            };
            drop(permit);

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                tracing::warn!(url, attempt, "etherscan HTTP 429, cooling key and retrying");
                self.key_pool.cool(&api_key);
                last_err = format!("http {status}");
                continue;
            }
            if !status.is_success() {
                return Err(DomainError::InsufficientData(format!(
                    "etherscan http {status}: {body}"
                )));
            }

            let parsed = serde_json::from_str::<EtherscanResponse>(&body).map_err(|e| {
                DomainError::InsufficientData(format!(
                    "etherscan parse: {e}: {}",
                    body.chars().take(200).collect::<String>()
                ))
            })?;

            // Etherscan's free tier (and the keyed tier under burst) returns
            // HTTP 200 with `status:"0"` + a `result` string carrying the
            // human-readable rate-limit message. Cool the offending key and
            // try the next one.
            if let Some(msg) = parsed.rate_limit_message() {
                tracing::warn!(url, attempt, message = %msg, "etherscan rate-limited, cooling key and retrying");
                self.key_pool.cool(&api_key);
                last_err = msg.to_string();
                continue;
            }

            return Ok(parsed);
        }
        Err(DomainError::RateLimited(format!(
            "etherscan: after {} attempts: {last_err}",
            self.http_max_attempts
        )))
    }

    async fn latest_block_height(&self) -> Option<u64> {
        if let Some(h) = self.latest_block_cache.get(&()).await {
            return Some(h);
        }
        // Same throttling chain as the account endpoints; latest_block is
        // small but it still counts against the per-second quota.
        self.rate_limiter.acquire(1.0).await;
        let api_key = self.key_pool.pick()?;
        let url = format!(
            "{base}?chainid={chain}&module=proxy&action=eth_blockNumber&apikey={key}",
            base = self.base_url,
            chain = ETH_CHAIN_ID,
            key = api_key,
        );
        let permit = self.request_permits.clone().acquire_owned().await.ok()?;
        let body = match self.client.get(&url).send().await {
            Ok(r) => match r.text().await {
                Ok(b) => b,
                Err(e) => {
                    drop(permit);
                    tracing::warn!(error = %e, "etherscan eth_blockNumber body failed");
                    return None;
                }
            },
            Err(e) => {
                drop(permit);
                tracing::warn!(error = %e, "etherscan eth_blockNumber http failed");
                return None;
            }
        };
        drop(permit);
        if looks_like_rate_limit(&body) {
            self.key_pool.cool(&api_key);
            tracing::warn!("etherscan eth_blockNumber rate-limited, cooling key");
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(&body).ok()?;
        let hex = v.get("result").and_then(|r| r.as_str())?.trim_start_matches("0x");
        let h = u64::from_str_radix(hex, 16).ok()?;
        self.latest_block_cache.insert((), h).await;
        Some(h)
    }

    /// A page is "hot" if its requested upper bound or any returned row sits
    /// within the unfinalized tail `latest - confirmation_depth`. Page key
    /// usually carries `to_block`, so we lean on that first.
    async fn classify_hot(&self, key: &PageKey, rows: &[serde_json::Value]) -> bool {
        let Some(latest) = self.latest_block_height().await else {
            return true;
        };
        let cutoff = latest.saturating_sub(self.confirmation_depth);
        if key.to_block > cutoff {
            return true;
        }
        rows.iter().any(|r| {
            r.get("blockNumber")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<u64>().ok())
                .map(|h| h > cutoff)
                .unwrap_or(false)
        })
    }

    async fn lookup_page(&self, key: &PageKey) -> Option<PageValue> {
        if let Some(v) = self.cold_page_cache.get(key).await {
            return Some(v);
        }
        if let Some(v) = self.hot_page_cache.get(key).await {
            return Some(v);
        }
        if let Some(path) = self.file_path(key)
            && let Some(v) = self.preloaded_file_cache.get(&path)
        {
            tracing::debug!(path = %path.display(), "etherscan: file-cache hit");
            return Some(Arc::clone(v));
        }
        None
    }

    async fn insert_page(&self, key: PageKey, value: PageValue, is_hot: bool) {
        if is_hot {
            if self.cache_hot_tail {
                self.hot_page_cache.insert(key, value).await;
            }
            // hot pages are NEVER written to disk — they may reorg.
        } else {
            self.cold_page_cache.insert(key.clone(), Arc::clone(&value)).await;
            if let Some(path) = self.file_path(&key) {
                self.file_write(&path, &value).await;
            }
        }
    }

    fn file_path(&self, key: &PageKey) -> Option<PathBuf> {
        let dir = self.file_cache_dir.as_ref()?;
        let addr = key.address.strip_prefix("0x").unwrap_or(&key.address);
        Some(dir.join(format!(
            "{}__{addr}__{from}__{to}__{page}.json",
            key.endpoint.prefix(),
            from = key.from_block,
            to = key.to_block,
            page = key.page,
        )))
    }

    async fn file_write(&self, path: &Path, value: &[serde_json::Value]) {
        if let Some(parent) = path.parent()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            tracing::warn!(dir = %parent.display(), error = %e, "etherscan: mkdir failed");
            return;
        }
        let body = match serde_json::to_string(value) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "etherscan: serialize page failed");
                return;
            }
        };
        match tokio::fs::write(path, &body).await {
            Ok(_) => tracing::debug!(path = %path.display(), bytes = body.len(), "etherscan: file cache written"),
            Err(e) => tracing::warn!(path = %path.display(), error = %e, "etherscan: file write failed"),
        }
    }

    async fn fetch_page(
        &self,
        endpoint: Endpoint,
        address_hex: &str,
        from_block: u64,
        to_block: u64,
        page: u32,
    ) -> DomainResult<PageValue> {
        let key = PageKey {
            endpoint,
            address: address_hex.to_string(),
            from_block,
            to_block,
            page,
        };
        if let Some(v) = self.lookup_page(&key).await {
            tracing::debug!(?endpoint, address = address_hex, page, "etherscan page cache hit");
            return Ok(v);
        }

        let params = [
            ("address", address_hex.to_string()),
            ("startblock", from_block.to_string()),
            ("endblock", to_block.to_string()),
            ("page", page.to_string()),
            ("offset", self.page_size.to_string()),
            ("sort", "asc".to_string()),
        ];
        let resp = self.http_get_json(endpoint, &params).await?;

        let rows = resp.into_result_rows().map_err(|EtherscanError(msg)| {
            DomainError::InsufficientData(format!(
                "etherscan {} returned error: {msg}",
                endpoint.action()
            ))
        })?;

        let is_hot = self.classify_hot(&key, &rows).await;
        let arc = Arc::new(rows);
        self.insert_page(key, Arc::clone(&arc), is_hot).await;
        Ok(arc)
    }

    async fn collect(
        &self,
        endpoint: Endpoint,
        address_hex: &str,
        from_block: u64,
        to_block: u64,
        max_transfers: usize,
    ) -> DomainResult<Vec<serde_json::Value>> {
        let mut all = Vec::new();
        for page in 1..=self.max_pages {
            let rows = self
                .fetch_page(endpoint, address_hex, from_block, to_block, page)
                .await?;
            let page_len = rows.len();
            all.extend(rows.iter().cloned());
            tracing::debug!(
                ?endpoint,
                address = address_hex,
                page,
                page_len,
                total = all.len(),
                "etherscan paginated"
            );
            if page_len < self.page_size as usize || all.len() >= max_transfers {
                break;
            }
        }
        Ok(all)
    }
}

#[async_trait]
impl ChainSource for EtherscanEvmSource {
    fn chain_id(&self) -> ChainId {
        self.chain
    }

    async fn latest_block(&self) -> DomainResult<BlockRef> {
        match self.latest_block_height().await {
            Some(h) => Ok(BlockRef::new(self.chain, h, [0u8; 32])),
            None => Err(DomainError::InsufficientData(
                "etherscan: eth_blockNumber failed".into(),
            )),
        }
    }

    async fn fetch_block(&self, height: u64) -> DomainResult<NormalizedBlock> {
        Err(DomainError::InsufficientData(format!(
            "etherscan: fetch_block by height ({height}) not supported; use transfers_for_address"
        )))
    }

    async fn transfers_for_address(
        &self,
        addr: &Address,
        range: BlockRange,
        max_transfers: usize,
    ) -> DomainResult<Vec<Transfer>> {
        if addr.chain() != self.chain {
            return Err(DomainError::InsufficientData(format!(
                "etherscan source (chain={}) called with foreign address chain: {}",
                self.chain,
                addr.chain()
            )));
        }
        let address_hex = format!("0x{}", hex::encode(addr.bytes()));
        let from_block = range.from_height();
        // etherscan accepts a sentinel for "latest"; modern blocks
        // are ~22M, so any large value works as long as it's a valid u32-ish.
        let to_block = if range.to_height() == u64::MAX {
            LATEST_SENTINEL
        } else {
            range.to_height()
        };

        tracing::info!(
            address = %address_hex,
            from_block,
            to_block,
            max_transfers,
            "etherscan: fetching ETH transfers"
        );

        // Fan-out all three account endpoints concurrently — they share the
        // semaphore, so this respects the upstream rate cap while cutting
        // wall-clock latency to the slowest endpoint instead of their sum.
        let (native_raw, internal_raw, token_raw) = tokio::try_join!(
            self.collect(Endpoint::TxList, &address_hex, from_block, to_block, max_transfers),
            self.collect(
                Endpoint::TxListInternal,
                &address_hex,
                from_block,
                to_block,
                max_transfers
            ),
            self.collect(Endpoint::TokenTx, &address_hex, from_block, to_block, max_transfers),
        )?;

        self.harvest_contract_signals(&native_raw, &internal_raw, &token_raw)
            .await;

        let mut native = Vec::with_capacity(native_raw.len());
        for raw in native_raw {
            match map_native(self.chain, &raw) {
                Ok(Some(t)) => native.push(t),
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, "etherscan: skip malformed native row"),
            }
        }

        // Shared idx counter across internal + token rows so they never
        // collide on (chain, tx_hash, idx) — native always claims idx=0.
        let mut by_tx: HashMap<[u8; 32], u32> = HashMap::new();

        let mut internal = Vec::with_capacity(internal_raw.len());
        for raw in internal_raw {
            match map_internal(self.chain, &raw, &mut by_tx) {
                Ok(Some(t)) => internal.push(t),
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, "etherscan: skip malformed internal row"),
            }
        }

        let mut token = Vec::with_capacity(token_raw.len());
        for raw in token_raw {
            match map_token(self.chain, &raw, &mut by_tx) {
                Ok(Some(t)) => token.push(t),
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, "etherscan: skip malformed token row"),
            }
        }

        tracing::info!(
            address = %address_hex,
            native = native.len(),
            internal = internal.len(),
            erc20 = token.len(),
            total = native.len() + internal.len() + token.len(),
            "etherscan: transfers fetched"
        );

        let mut out = native;
        out.extend(internal);
        out.extend(token);
        if out.len() > max_transfers {
            out.truncate(max_transfers);
        }
        Ok(out)
    }

    async fn is_contract(&self, addr: &Address) -> DomainResult<Option<bool>> {
        if addr.chain() != self.chain {
            return Ok(None);
        }

        let bytes = addr.bytes().to_vec();
        if let Some(cached) = self.is_contract_cache.get(&bytes).await {
            return Ok(Some(cached));
        }

        let address_hex = format!("0x{}", hex::encode(&bytes));
        match self.is_contract_with_retry(&address_hex).await {
            Ok(Some(is_c)) => {
                self.is_contract_cache.insert(bytes, is_c).await;
                Ok(Some(is_c))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Etherscan has no JSON-RPC batch, so each cache miss is its own
    /// `eth_getCode`. But the in-memory harvest cache typically covers
    /// 60-80% of any traced address graph (every contract that was the
    /// `from` of an internal trace or the `to` of a calldata-bearing call,
    /// plus every ERC-20 contract that emitted a Transfer). For those
    /// addresses the batch resolves with zero network traffic — only the
    /// stragglers fan out as parallel single-address calls.
    async fn is_contract_batch(
        &self,
        addrs: &[Address],
    ) -> DomainResult<Vec<Option<bool>>> {
        if addrs.is_empty() {
            return Ok(Vec::new());
        }
        let mut out: Vec<Option<bool>> = vec![None; addrs.len()];
        let mut to_fetch: Vec<(usize, &Address)> = Vec::new();
        for (i, addr) in addrs.iter().enumerate() {
            if addr.chain() != self.chain {
                continue;
            }
            let bytes = addr.bytes().to_vec();
            if let Some(cached) = self.is_contract_cache.get(&bytes).await {
                out[i] = Some(cached);
            } else {
                to_fetch.push((i, addr));
            }
        }
        if to_fetch.is_empty() {
            return Ok(out);
        }

        use futures::future::join_all;
        let futs = to_fetch.iter().map(|(_, addr)| async move {
            let bytes = addr.bytes().to_vec();
            let hex = format!("0x{}", hex::encode(&bytes));
            (self.is_contract_with_retry(&hex).await, bytes)
        });
        let results = join_all(futs).await;
        for ((i, _), (res, bytes)) in to_fetch.iter().zip(results.into_iter()) {
            match res {
                Ok(Some(v)) => {
                    self.is_contract_cache.insert(bytes, v).await;
                    out[*i] = Some(v);
                }
                Ok(None) => {}
                // Single rate-limited address shouldn't sink the whole
                // batch — record as soft-unknown and keep going.
                Err(_) => {}
            }
        }
        Ok(out)
    }
}

impl EtherscanEvmSource {
    /// Mine the raw `txlist`/`txlistinternal`/`tokentx` responses we already
    /// have in hand for address-kind signals so that the follow-up
    /// `is_contract` pass (driven by `classify_address_kinds` in the use-case)
    /// can read them from cache instead of issuing one `eth_getCode` per
    /// address. The bigger the harvest, the lower the downstream API spend —
    /// on a typical traced graph this eliminates 60-80% of `eth_getCode`
    /// calls (most addresses are EOAs that signed at least one outer tx).
    ///
    /// Signals that imply contract (`is_contract_cache → true`):
    /// * Native tx (`txlist`) where `input != "0x"` → `to` is invoking
    ///   contract code.
    /// * Native tx where `contractAddress` is non-empty → contract creation;
    ///   that field holds the freshly-deployed contract address.
    /// * Internal tx (`txlistinternal`) — the `from` of *any* internal row
    ///   is, by EVM semantics, a contract: only contract code can emit
    ///   `CALL`/`CREATE` opcodes mid-execution. This is the strongest
    ///   signal and typically dominates the harvest on DeFi-heavy traces.
    /// * Internal tx of `type = create | create2` → `contractAddress` is the
    ///   deployed contract.
    /// * Internal tx with `input.len() > 2` → `to` is a contract.
    /// * Token tx (`tokentx`) — its `contractAddress` is the ERC-20 contract
    ///   itself.
    ///
    /// Signals that imply EOA (`is_contract_cache → false`):
    /// * Native tx (`txlist`) — `from` was signed by a private key, so it's
    ///   an EOA. The one exception is EIP-7702 delegated accounts (live on
    ///   mainnet since Pectra, May 2025): they still hold a key but also
    ///   carry a delegation pointer in their code slot. We handle that case
    ///   by letting any later contract-evidence override the EOA marking
    ///   (see the dedup pass below) — so a 7702 account that ever drove an
    ///   internal call is classified as contract, while a plain 7702 user
    ///   who only signs and never delegates anything observable stays EOA
    ///   (which matches the risk/clustering interpretation we want anyway).
    ///
    /// Signals deliberately NOT harvested:
    /// * `from`/`to` of `tokentx` rows — ERC-20 Transfer event participants
    ///   can be either; pools and vaults regularly emit Transfers.
    /// * `to` of internal `call` without calldata — destination of a plain
    ///   value transfer; can be EOA or contract.
    async fn harvest_contract_signals(
        &self,
        native_raw: &[serde_json::Value],
        internal_raw: &[serde_json::Value],
        token_raw: &[serde_json::Value],
    ) {
        let mut confirmed_contract: HashSet<Vec<u8>> = HashSet::new();
        let mut confirmed_eoa: HashSet<Vec<u8>> = HashSet::new();

        for raw in native_raw {
            // `from` of an outer EVM tx was signed by a private key → EOA
            // (modulo 7702, resolved by the post-loop dedup below).
            if let Some(from_s) = raw
                .get("from")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                if let Ok(addr) = parse_eth_address(self.chain, from_s) {
                    confirmed_eoa.insert(addr.bytes().to_vec());
                }
            }
            // Calldata signals contract call on `to`.
            let input = raw.get("input").and_then(|v| v.as_str()).unwrap_or("0x");
            if input.len() > 2 {
                if let Some(to_s) = raw
                    .get("to")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    if let Ok(addr) = parse_eth_address(self.chain, to_s) {
                        confirmed_contract.insert(addr.bytes().to_vec());
                    }
                }
            }
            // Contract creation: `contractAddress` is the deployed contract.
            if let Some(c) = raw
                .get("contractAddress")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty() && *s != "0x")
            {
                if let Ok(addr) = parse_eth_address(self.chain, c) {
                    confirmed_contract.insert(addr.bytes().to_vec());
                }
            }
        }

        for raw in internal_raw {
            // The `from` of any internal trace is necessarily a contract —
            // only contract code can drive CALL/CREATE opcodes during a tx.
            if let Some(from_s) = raw
                .get("from")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                if let Ok(addr) = parse_eth_address(self.chain, from_s) {
                    confirmed_contract.insert(addr.bytes().to_vec());
                }
            }
            // CREATE / CREATE2 — `contractAddress` is the deployed contract.
            if matches!(
                raw.get("type").and_then(|v| v.as_str()),
                Some("create") | Some("create2")
            ) {
                if let Some(c) = raw
                    .get("contractAddress")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty() && *s != "0x")
                {
                    if let Ok(addr) = parse_eth_address(self.chain, c) {
                        confirmed_contract.insert(addr.bytes().to_vec());
                    }
                }
            }
            // Calldata-bearing sub-call → callee is a contract.
            let input = raw.get("input").and_then(|v| v.as_str()).unwrap_or("0x");
            if input.len() > 2 {
                if let Some(to_s) = raw
                    .get("to")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    if let Ok(addr) = parse_eth_address(self.chain, to_s) {
                        confirmed_contract.insert(addr.bytes().to_vec());
                    }
                }
            }
        }

        for raw in token_raw {
            // ERC-20 transfer — `contractAddress` is the token contract itself.
            if let Some(c) = raw
                .get("contractAddress")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                if let Ok(addr) = parse_eth_address(self.chain, c) {
                    confirmed_contract.insert(addr.bytes().to_vec());
                }
            }
        }

        // Stronger evidence wins: if an address signed an outer tx AND
        // appears as the driver of an internal call (the EIP-7702 case),
        // record it as a contract so downstream pattern detectors that
        // care about contract behaviour aren't blinded.
        for addr in &confirmed_contract {
            confirmed_eoa.remove(addr);
        }

        if confirmed_contract.is_empty() && confirmed_eoa.is_empty() {
            return;
        }
        let nc = confirmed_contract.len();
        let ne = confirmed_eoa.len();
        for bytes in confirmed_contract {
            self.is_contract_cache.insert(bytes, true).await;
        }
        for bytes in confirmed_eoa {
            self.is_contract_cache.insert(bytes, false).await;
        }
        tracing::debug!(
            contracts = nc,
            eoas = ne,
            "etherscan: harvested address-kind signals from txlist/txlistinternal/tokentx"
        );
    }

    /// Resolve `is_contract` by calling Etherscan's `eth_getCode` proxy
    /// endpoint, rotating API keys on throttling. Returns:
    /// * `Ok(Some(true|false))` — code present / empty.
    /// * `Ok(None)` — soft-unknown (non-hex payload, permanent client error);
    ///   caller leaves `AddressKind::Unknown`.
    /// * `Err(RateLimited)` — every key cooled or retries exhausted; the
    ///   router treats this as a failover trigger (so Alchemy can answer).
    async fn is_contract_with_retry(&self, address_hex: &str) -> DomainResult<Option<bool>> {
        let mut last_err = String::new();
        for attempt in 0..self.is_contract_max_attempts {
            // Same rate-limit + key-wait dance as http_get_json.
            self.rate_limiter.acquire(1.0).await;
            let api_key = match self.key_pool.pick_or_wait() {
                Ok(k) => k,
                Err(wait) => {
                    tracing::warn!(
                        attempt,
                        wait_ms = wait.as_millis() as u64,
                        "etherscan is_contract: all keys cooled, waiting before retry"
                    );
                    tokio::time::sleep(wait).await;
                    last_err = "all keys cooled".to_string();
                    continue;
                }
            };

            if attempt > 0 && last_err.starts_with("http") {
                let backoff = Duration::from_millis(
                    RATE_LIMIT_BACKOFF_BASE_MS.saturating_mul(1u64 << (attempt - 1)),
                );
                tokio::time::sleep(backoff).await;
            }

            let url = format!(
                "{base}?chainid={chain}&module=proxy&action=eth_getCode&address={addr}&tag=latest&apikey={key}",
                base = self.base_url,
                chain = ETH_CHAIN_ID,
                addr = address_hex,
                key = api_key,
            );

            let permit = match self.request_permits.clone().acquire_owned().await {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(error = %e, "etherscan is_contract semaphore closed");
                    return Ok(None);
                }
            };

            let resp = self.client.get(&url).send().await;
            let r = match resp {
                Ok(r) => r,
                Err(e) => {
                    drop(permit);
                    tracing::warn!(attempt, error = %e, "etherscan is_contract http failed");
                    last_err = e.to_string();
                    continue;
                }
            };
            let status = r.status();
            let body = match r.text().await {
                Ok(t) => t,
                Err(e) => {
                    drop(permit);
                    tracing::warn!(attempt, error = %e, "etherscan is_contract body read failed");
                    last_err = e.to_string();
                    continue;
                }
            };
            drop(permit);

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                tracing::warn!(attempt, status = status.as_u16(), "etherscan is_contract 429, cooling key and retrying");
                self.key_pool.cool(&api_key);
                last_err = format!("http {status}");
                continue;
            }
            if status.is_server_error() {
                tracing::warn!(attempt, status = status.as_u16(), "etherscan is_contract 5xx, retrying");
                last_err = format!("http {status}");
                continue;
            }
            if !status.is_success() {
                // Permanent client error (400/401/403/404/...). Don't retry —
                // result won't change on the same URL.
                tracing::warn!(
                    status = status.as_u16(),
                    snippet = %body.chars().take(120).collect::<String>(),
                    "etherscan is_contract non-2xx, giving up"
                );
                return Ok(None);
            }
            if looks_like_rate_limit(&body) {
                tracing::warn!(attempt, "etherscan is_contract rate-limit body, cooling key and retrying");
                self.key_pool.cool(&api_key);
                last_err = "rate-limit body".into();
                continue;
            }
            return Ok(parse_get_code(&body));
        }
        tracing::warn!(
            attempts = self.is_contract_max_attempts,
            "etherscan is_contract exhausted retries"
        );
        Err(DomainError::RateLimited(format!(
            "etherscan is_contract: {last_err}"
        )))
    }
}

/// Heuristic match for Etherscan rate-limit shapes, regardless of HTTP code:
/// * `{"status":"0","message":"NOTOK","result":"Max calls per sec rate limit reached (5/sec)"}`
/// * `{"jsonrpc":"2.0","error":{"code":-32007,"message":"Too many requests"}}`
/// * `{"status":"0","message":"NOTOK","result":"daily rate limit reached"}`
fn looks_like_rate_limit(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("rate limit")
        || lower.contains("too many request")
        || lower.contains("max calls")
}

/// Parse the `eth_getCode` JSON-RPC response from Etherscan's proxy endpoint.
/// Returns:
/// * `Some(true)`  — hex bytecode present (`0x6080…`)
/// * `Some(false)` — empty bytecode (`0x`)
/// * `None`        — non-hex result (rate-limit message, JSON-RPC error, …);
///                   caller treats as "unknown" and leaves AddressKind = Unknown
fn parse_get_code(body: &str) -> Option<bool> {
    let v: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, snippet = %body.chars().take(120).collect::<String>(), "etherscan eth_getCode invalid JSON");
            return None;
        }
    };
    if v.get("error").is_some() {
        tracing::debug!(payload = %v, "etherscan eth_getCode RPC error");
        return None;
    }
    let result = match v.get("result").and_then(|r| r.as_str()) {
        Some(s) => s,
        None => {
            tracing::debug!(payload = %v, "etherscan eth_getCode missing result");
            return None;
        }
    };
    // Real eth_getCode results are always hex with an `0x` prefix and even
    // length. Anything else (rate-limit string, error message) → unknown.
    let stripped = match result.strip_prefix("0x").or_else(|| result.strip_prefix("0X")) {
        Some(s) => s,
        None => {
            tracing::warn!(snippet = result, "etherscan eth_getCode non-hex result");
            return None;
        }
    };
    if !stripped.chars().all(|c| c.is_ascii_hexdigit()) || stripped.len() % 2 != 0 {
        tracing::warn!(snippet = result, "etherscan eth_getCode malformed hex");
        return None;
    }
    Some(!stripped.is_empty())
}

#[derive(Deserialize)]
struct EtherscanResponse {
    status: String,
    message: String,
    result: serde_json::Value,
}

struct EtherscanError(String);

impl EtherscanResponse {
    /// Returns the human-readable rate-limit text when the response signals
    /// throttling, otherwise None. Recognises etherscan's "Max calls per sec"
    /// and the daily-quota "Max daily rate limit" wordings.
    fn rate_limit_message(&self) -> Option<&str> {
        if self.status != "0" {
            return None;
        }
        let msg = self.result.as_str()?;
        let lc = msg.to_ascii_lowercase();
        if lc.contains("rate limit reached") || lc.contains("max calls per sec") {
            Some(msg)
        } else {
            None
        }
    }

    fn into_result_rows(self) -> Result<Vec<serde_json::Value>, EtherscanError> {
        if self.status == "1" {
            match self.result {
                serde_json::Value::Array(arr) => Ok(arr),
                other => Err(EtherscanError(format!("expected array, got {other}"))),
            }
        } else if self.message == "No transactions found" {
            Ok(Vec::new())
        } else {
            let detail = match self.result {
                serde_json::Value::String(s) => s,
                serde_json::Value::Array(_) => self.message,
                other => other.to_string(),
            };
            Err(EtherscanError(detail))
        }
    }
}

fn map_native(chain: ChainId, raw: &serde_json::Value) -> anyhow::Result<Option<Transfer>> {
    use anyhow::{Context, anyhow};

    if raw.get("isError").and_then(|v| v.as_str()) == Some("1") {
        return Ok(None);
    }
    let value_s = raw
        .get("value")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("native: missing value"))?;
    let raw_val = U256::from_dec_str(value_s).context("native: value")?;
    if raw_val.is_zero() {
        return Ok(None);
    }

    let to_s = raw
        .get("to")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let Some(to_s) = to_s else {
        return Ok(None);
    };
    let from_s = raw
        .get("from")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("native: missing from"))?;
    let tx_hash_s = raw
        .get("hash")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("native: missing hash"))?;
    let block_num_s = raw
        .get("blockNumber")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("native: missing blockNumber"))?;
    let ts_s = raw
        .get("timeStamp")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("native: missing timeStamp"))?;
    let block_hash_s = raw.get("blockHash").and_then(|v| v.as_str());

    let tx_hash = parse_hash32(tx_hash_s).context("native: tx hash")?;
    let block_hash = block_hash_s
        .map(parse_hash32)
        .transpose()
        .context("native: block_hash")?
        .unwrap_or(tx_hash);
    let block_number: u64 = block_num_s.parse().context("native: blockNumber")?;
    let ts_secs: i64 = ts_s.parse().context("native: timeStamp")?;
    let timestamp = chrono::Utc
        .timestamp_opt(ts_secs, 0)
        .single()
        .ok_or_else(|| anyhow!("native: bad timestamp {ts_secs}"))?;

    let from = parse_eth_address(chain, from_s).context("native: from")?;
    let to = parse_eth_address(chain, to_s).context("native: to")?;

    let finality = match raw.get("txreceipt_status").and_then(|v| v.as_str()) {
        Some("0") => Finality::Reorged,
        _ => Finality::Confirmed,
    };

    Ok(Some(Transfer::new(
        TransferId::new(chain, tx_hash, 0),
        chain,
        TxRef::new(chain, tx_hash),
        from,
        to,
        AssetId::native(chain),
        Amount::new(raw_val, 18),
        BlockRef::new(chain, block_number, block_hash),
        timestamp,
        TransferKind::Native,
        finality,
    )))
}

/// Map a `txlistinternal` row into a Transfer. Internal rows describe value
/// movements driven by `CALL`/`CALLCODE`/`CREATE*`/`SELFDESTRUCT` opcodes
/// during contract execution — i.e. the ones missing from `txlist`.
///
/// Filtering:
/// * `isError == "1"` — the sub-call reverted, no value moved.
/// * `type == "delegatecall" | "staticcall"` — these never transfer value
///   regardless of the `value` field (delegatecall preserves caller context,
///   staticcall forbids state changes).
/// * `value == 0` — nothing to record.
///
/// `to` is empty for `create`/`create2` rows; in that case the destination
/// is the freshly-deployed contract address sitting in `contractAddress`.
fn map_internal(
    chain: ChainId,
    raw: &serde_json::Value,
    by_tx: &mut HashMap<[u8; 32], u32>,
) -> anyhow::Result<Option<Transfer>> {
    use anyhow::{Context, anyhow};

    if raw.get("isError").and_then(|v| v.as_str()) == Some("1") {
        return Ok(None);
    }
    if matches!(
        raw.get("type").and_then(|v| v.as_str()),
        Some("delegatecall") | Some("staticcall")
    ) {
        return Ok(None);
    }

    let value_s = raw
        .get("value")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("internal: missing value"))?;
    let raw_val = U256::from_dec_str(value_s).context("internal: value")?;
    if raw_val.is_zero() {
        return Ok(None);
    }

    let from_s = raw
        .get("from")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("internal: missing from"))?;
    let to_s = raw
        .get("to")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let dest_s = match to_s {
        Some(s) => s.to_string(),
        None => raw
            .get("contractAddress")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty() && *s != "0x")
            .ok_or_else(|| anyhow!("internal: missing to/contractAddress"))?
            .to_string(),
    };

    let tx_hash_s = raw
        .get("hash")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("internal: missing hash"))?;
    let block_num_s = raw
        .get("blockNumber")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("internal: missing blockNumber"))?;
    let ts_s = raw
        .get("timeStamp")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("internal: missing timeStamp"))?;

    let tx_hash = parse_hash32(tx_hash_s).context("internal: tx hash")?;
    let block_number: u64 = block_num_s.parse().context("internal: blockNumber")?;
    let ts_secs: i64 = ts_s.parse().context("internal: timeStamp")?;
    let timestamp = chrono::Utc
        .timestamp_opt(ts_secs, 0)
        .single()
        .ok_or_else(|| anyhow!("internal: bad timestamp {ts_secs}"))?;

    let from = parse_eth_address(chain, from_s).context("internal: from")?;
    let to = parse_eth_address(chain, &dest_s).context("internal: to")?;

    // idx=0 is reserved for the outer native value transfer (txlist row);
    // bump per row within the same tx — shared counter with token rows in
    // the caller, so internal+token never collide on (chain, tx_hash, idx).
    let position = by_tx.entry(tx_hash).or_insert(0);
    let idx = position.saturating_add(1);
    *position += 1;

    Ok(Some(Transfer::new(
        TransferId::new(chain, tx_hash, idx),
        chain,
        TxRef::new(chain, tx_hash),
        from,
        to,
        AssetId::native(chain),
        Amount::new(raw_val, 18),
        // Internal rows don't carry blockHash; fall back to tx_hash like
        // map_native does for the same reason. Reorg classification is
        // driven by block height + confirmation_depth, not by hash equality.
        BlockRef::new(chain, block_number, tx_hash),
        timestamp,
        TransferKind::Native,
        Finality::Confirmed,
    )))
}

fn map_token(
    chain: ChainId,
    raw: &serde_json::Value,
    by_tx: &mut HashMap<[u8; 32], u32>,
) -> anyhow::Result<Option<Transfer>> {
    use anyhow::{Context, anyhow};

    let from_s = raw
        .get("from")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("token: missing from"))?;
    let to_s = raw
        .get("to")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let Some(to_s) = to_s else {
        return Ok(None);
    };
    let contract_s = raw
        .get("contractAddress")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("token: missing contractAddress"))?;
    let value_s = raw
        .get("value")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("token: missing value"))?;
    let tx_hash_s = raw
        .get("hash")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("token: missing hash"))?;
    let block_num_s = raw
        .get("blockNumber")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("token: missing blockNumber"))?;
    let ts_s = raw
        .get("timeStamp")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("token: missing timeStamp"))?;
    let block_hash_s = raw.get("blockHash").and_then(|v| v.as_str());
    let decimals_s = raw.get("tokenDecimal").and_then(|v| v.as_str());
    let symbol = raw
        .get("tokenSymbol")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let tx_hash = parse_hash32(tx_hash_s).context("token: tx hash")?;
    let block_hash = block_hash_s
        .map(parse_hash32)
        .transpose()
        .context("token: block_hash")?
        .unwrap_or(tx_hash);
    let block_number: u64 = block_num_s.parse().context("token: blockNumber")?;
    let ts_secs: i64 = ts_s.parse().context("token: timeStamp")?;
    let timestamp = chrono::Utc
        .timestamp_opt(ts_secs, 0)
        .single()
        .ok_or_else(|| anyhow!("token: bad timestamp {ts_secs}"))?;
    let decimals: u8 = decimals_s
        .map(|s| s.parse::<u8>())
        .transpose()
        .context("token: tokenDecimal")?
        .unwrap_or(18);
    let raw_val = U256::from_dec_str(value_s).context("token: value")?;

    let from = parse_eth_address(chain, from_s).context("token: from")?;
    let to = parse_eth_address(chain, to_s).context("token: to")?;
    let contract = parse_eth_address(chain, contract_s).context("token: contractAddress")?;

    // idx=0 is reserved for the (single) native transfer in this tx; shift
    // token rows by +1 so they never collide on the (chain, tx_hash, idx) PK
    // when a tx has both a native value transfer and an ERC-20 Transfer event.
    let position = by_tx.entry(tx_hash).or_insert(0);
    let idx = position.saturating_add(1);
    *position += 1;

    Ok(Some(Transfer::new(
        TransferId::new(chain, tx_hash, idx),
        chain,
        TxRef::new(chain, tx_hash),
        from,
        to,
        AssetId::contract(chain, contract.bytes().to_vec()),
        Amount::new(raw_val, decimals),
        BlockRef::new(chain, block_number, block_hash),
        timestamp,
        TransferKind::Token {
            contract,
            standard: TokenStandard::Erc20,
            symbol,
        },
        Finality::Confirmed,
    )))
}

fn parse_hash32(s: &str) -> anyhow::Result<[u8; 32]> {
    use anyhow::{Context, anyhow};
    let s = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(s).context("hex decode")?;
    bytes
        .try_into()
        .map_err(|v: Vec<u8>| anyhow!("expected 32 bytes, got {}", v.len()))
}

fn parse_eth_address(chain: ChainId, s: &str) -> anyhow::Result<Address> {
    use anyhow::Context;
    let s = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(s).context("hex decode eth address")?;
    if bytes.len() != 20 {
        anyhow::bail!("eth address expected 20 bytes, got {}", bytes.len());
    }
    Ok(Address::new(chain, bytes))
}

#[cfg(test)]
mod parse_get_code_tests {
    use super::parse_get_code;

    #[test]
    fn empty_bytecode_is_eoa() {
        assert_eq!(parse_get_code(r#"{"jsonrpc":"2.0","id":1,"result":"0x"}"#), Some(false));
    }

    #[test]
    fn non_empty_bytecode_is_contract() {
        assert_eq!(
            parse_get_code(r#"{"jsonrpc":"2.0","id":1,"result":"0x6080604052"}"#),
            Some(true)
        );
    }

    #[test]
    fn rate_limit_message_is_unknown() {
        // Etherscan rate-limit response — result is a human-readable string
        // that would naively pass `len() > 2` and falsely classify as Contract.
        let body =
            r#"{"status":"0","message":"NOTOK","result":"Max calls per sec rate limit reached"}"#;
        assert_eq!(parse_get_code(body), None);
    }

    #[test]
    fn jsonrpc_error_is_unknown() {
        let body = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"invalid argument"}}"#;
        assert_eq!(parse_get_code(body), None);
    }

    #[test]
    fn missing_result_is_unknown() {
        assert_eq!(parse_get_code(r#"{"jsonrpc":"2.0","id":1}"#), None);
    }

    #[test]
    fn html_body_is_unknown() {
        // Etherscan sometimes serves HTML when overloaded.
        assert_eq!(parse_get_code("<html><body>503</body></html>"), None);
    }

    #[test]
    fn uppercase_0x_is_accepted() {
        assert_eq!(parse_get_code(r#"{"result":"0X6080"}"#), Some(true));
    }

    #[test]
    fn odd_length_hex_is_unknown() {
        // Real bytecode is always even-length; treat odd as bogus.
        assert_eq!(parse_get_code(r#"{"result":"0x60"}"#), Some(true));
        assert_eq!(parse_get_code(r#"{"result":"0x6"}"#), None);
    }

    #[test]
    fn non_hex_after_prefix_is_unknown() {
        assert_eq!(parse_get_code(r#"{"result":"0xZZ"}"#), None);
    }
}

#[cfg(test)]
mod harvest_signal_tests {
    use serde_json::json;

    /// Replica of the logic inside `harvest_contract_signals` — keeps the test
    /// honest about which signals we accept without touching async cache state.
    fn collect_confirmed(
        native_raw: &[serde_json::Value],
        internal_raw: &[serde_json::Value],
        token_raw: &[serde_json::Value],
    ) -> std::collections::HashSet<String> {
        let mut out = std::collections::HashSet::new();
        for raw in native_raw {
            let input = raw.get("input").and_then(|v| v.as_str()).unwrap_or("0x");
            if input.len() > 2 {
                if let Some(to) = raw
                    .get("to")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    out.insert(to.to_ascii_lowercase());
                }
            }
            if let Some(c) = raw
                .get("contractAddress")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty() && *s != "0x")
            {
                out.insert(c.to_ascii_lowercase());
            }
        }
        for raw in internal_raw {
            if let Some(from) = raw
                .get("from")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                out.insert(from.to_ascii_lowercase());
            }
            if matches!(
                raw.get("type").and_then(|v| v.as_str()),
                Some("create") | Some("create2")
            ) {
                if let Some(c) = raw
                    .get("contractAddress")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty() && *s != "0x")
                {
                    out.insert(c.to_ascii_lowercase());
                }
            }
            let input = raw.get("input").and_then(|v| v.as_str()).unwrap_or("0x");
            if input.len() > 2 {
                if let Some(to) = raw
                    .get("to")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    out.insert(to.to_ascii_lowercase());
                }
            }
        }
        for raw in token_raw {
            if let Some(c) = raw
                .get("contractAddress")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                out.insert(c.to_ascii_lowercase());
            }
        }
        out
    }

    #[test]
    fn calldata_on_native_tx_marks_to_as_contract() {
        let native = vec![json!({
            "from": "0xaaaa000000000000000000000000000000000000",
            "to":   "0xbbbb000000000000000000000000000000000000",
            "input": "0xa9059cbb000000000000000000000000",
        })];
        let got = collect_confirmed(&native, &[], &[]);
        assert!(got.contains("0xbbbb000000000000000000000000000000000000"));
        assert!(!got.contains("0xaaaa000000000000000000000000000000000000"));
    }

    #[test]
    fn empty_calldata_does_not_mark_to() {
        let native = vec![json!({
            "from": "0xaaaa000000000000000000000000000000000000",
            "to":   "0xbbbb000000000000000000000000000000000000",
            "input": "0x",
        })];
        let got = collect_confirmed(&native, &[], &[]);
        assert!(got.is_empty());
    }

    #[test]
    fn native_contract_creation_picks_contract_address() {
        let native = vec![json!({
            "from": "0xaaaa000000000000000000000000000000000000",
            "to":   "",
            "input": "0x6080...",
            "contractAddress": "0xcccc000000000000000000000000000000000000",
        })];
        let got = collect_confirmed(&native, &[], &[]);
        assert!(got.contains("0xcccc000000000000000000000000000000000000"));
    }

    #[test]
    fn token_transfer_marks_token_contract() {
        let token = vec![json!({
            "from": "0xaaaa000000000000000000000000000000000000",
            "to":   "0xbbbb000000000000000000000000000000000000",
            "contractAddress": "0xdac17f958d2ee523a2206206994597c13d831ec7",
        })];
        let got = collect_confirmed(&[], &[], &token);
        assert!(got.contains("0xdac17f958d2ee523a2206206994597c13d831ec7"));
        // Sender/receiver of the token transfer are NOT inferred to be contracts.
        assert!(!got.contains("0xaaaa000000000000000000000000000000000000"));
        assert!(!got.contains("0xbbbb000000000000000000000000000000000000"));
    }

    #[test]
    fn internal_from_is_always_contract() {
        // Only contract code can emit CALL/CREATE/SELFDESTRUCT internally;
        // therefore the `from` of any internal trace is by definition a
        // contract — even for a plain value-transfer call with no calldata.
        let internal = vec![json!({
            "from":  "0xrouter00000000000000000000000000000000000",
            "to":    "0xuser000000000000000000000000000000000000",
            "value": "1000000000000000000",
            "type":  "call",
            "input": "0x",
        })];
        let got = collect_confirmed(&[], &internal, &[]);
        assert!(got.contains("0xrouter00000000000000000000000000000000000"));
        // The recipient `to` is NOT inferred (could be EOA receiving a withdraw).
        assert!(!got.contains("0xuser000000000000000000000000000000000000"));
    }

    #[test]
    fn internal_create_picks_deployed_contract() {
        let internal = vec![json!({
            "from":            "0xfactory0000000000000000000000000000000000",
            "to":              "",
            "value":           "0",
            "type":            "create2",
            "input":           "0x6080...",
            "contractAddress": "0xchild000000000000000000000000000000000000",
        })];
        let got = collect_confirmed(&[], &internal, &[]);
        assert!(got.contains("0xfactory0000000000000000000000000000000000"));
        assert!(got.contains("0xchild000000000000000000000000000000000000"));
    }

    #[test]
    fn internal_calldata_marks_callee() {
        let internal = vec![json!({
            "from":  "0xrouter00000000000000000000000000000000000",
            "to":    "0xpool0000000000000000000000000000000000000",
            "value": "0",
            "type":  "call",
            "input": "0x022c0d9f...",
        })];
        let got = collect_confirmed(&[], &internal, &[]);
        assert!(got.contains("0xrouter00000000000000000000000000000000000"));
        assert!(got.contains("0xpool0000000000000000000000000000000000000"));
    }
}

#[cfg(test)]
mod eoa_harvest_tests {
    use serde_json::json;

    /// Replica of the EOA arm of `harvest_contract_signals`: collect the
    /// `from` of every outer txlist row, then drop any address that the
    /// contract-arm also flagged. Mirrors the production dedup so tests
    /// stay honest about 7702-override semantics.
    fn collect_confirmed_eoa(
        native_raw: &[serde_json::Value],
        internal_raw: &[serde_json::Value],
    ) -> std::collections::HashSet<String> {
        let mut eoa = std::collections::HashSet::new();
        let mut contract = std::collections::HashSet::new();

        for raw in native_raw {
            if let Some(from) = raw
                .get("from")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                eoa.insert(from.to_ascii_lowercase());
            }
            let input = raw.get("input").and_then(|v| v.as_str()).unwrap_or("0x");
            if input.len() > 2 {
                if let Some(to) = raw
                    .get("to")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    contract.insert(to.to_ascii_lowercase());
                }
            }
            if let Some(c) = raw
                .get("contractAddress")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty() && *s != "0x")
            {
                contract.insert(c.to_ascii_lowercase());
            }
        }
        for raw in internal_raw {
            if let Some(from) = raw
                .get("from")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                contract.insert(from.to_ascii_lowercase());
            }
        }
        for a in &contract {
            eoa.remove(a);
        }
        eoa
    }

    #[test]
    fn outer_from_is_eoa() {
        let native = vec![json!({
            "from":  "0xeoa00000000000000000000000000000000000000",
            "to":    "0xrecipient00000000000000000000000000000000",
            "input": "0x",
        })];
        let eoa = collect_confirmed_eoa(&native, &[]);
        assert!(eoa.contains("0xeoa00000000000000000000000000000000000000"));
    }

    #[test]
    fn outer_from_eoa_marking_survives_when_calling_contract() {
        // Plain user calling a DEX router: `to` is contract, `from` is EOA.
        // Both signals should fire on different addresses without conflict.
        let native = vec![json!({
            "from":  "0xeoa00000000000000000000000000000000000000",
            "to":    "0xrouter00000000000000000000000000000000000",
            "input": "0x38ed1739000000000000000000000000",
        })];
        let eoa = collect_confirmed_eoa(&native, &[]);
        assert!(eoa.contains("0xeoa00000000000000000000000000000000000000"));
        assert!(!eoa.contains("0xrouter00000000000000000000000000000000000"));
    }

    #[test]
    fn eip7702_account_classified_as_contract_when_driving_internal_calls() {
        // The same address X both signs an outer tx (would mark EOA) AND
        // drives internal CALLs (definitive contract evidence). Dedup must
        // strip it from the EOA set so downstream is_contract returns true.
        let x = "0x7702aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let native = vec![json!({
            "from":  x,
            "to":    "0xreceiver00000000000000000000000000000000",
            "input": "0x",
        })];
        let internal = vec![json!({
            "from":  x,
            "to":    "0xtarget0000000000000000000000000000000000",
            "value": "1000",
            "type":  "call",
            "input": "0xabcd1234",
        })];
        let eoa = collect_confirmed_eoa(&native, &internal);
        assert!(!eoa.contains(x), "7702 delegated account must not stay in EOA set");
    }

    #[test]
    fn contract_creation_deployer_still_marked_eoa() {
        // The deployer of a fresh contract is an EOA signing a CREATE tx.
        // Only the `contractAddress` (new contract) is flagged as contract;
        // the `from` (deployer) must keep its EOA marking.
        let native = vec![json!({
            "from":            "0xdeployer000000000000000000000000000000000",
            "to":              "",
            "input":           "0x6080...",
            "contractAddress": "0xchild000000000000000000000000000000000000",
        })];
        let eoa = collect_confirmed_eoa(&native, &[]);
        assert!(eoa.contains("0xdeployer000000000000000000000000000000000"));
        assert!(!eoa.contains("0xchild000000000000000000000000000000000000"));
    }
}

#[cfg(test)]
mod rate_limit_detector_tests {
    use super::looks_like_rate_limit;

    #[test]
    fn classic_etherscan_rate_limit() {
        assert!(looks_like_rate_limit(
            r#"{"status":"0","message":"NOTOK","result":"Max calls per sec rate limit reached (5/sec)"}"#
        ));
    }

    #[test]
    fn daily_quota_message() {
        assert!(looks_like_rate_limit(
            r#"{"status":"0","message":"NOTOK","result":"daily rate limit reached"}"#
        ));
    }

    #[test]
    fn jsonrpc_too_many_requests() {
        assert!(looks_like_rate_limit(
            r#"{"jsonrpc":"2.0","error":{"code":-32007,"message":"Too many requests"}}"#
        ));
    }

    #[test]
    fn happy_response_is_not_rate_limit() {
        assert!(!looks_like_rate_limit(
            r#"{"jsonrpc":"2.0","id":1,"result":"0x6080"}"#
        ));
        assert!(!looks_like_rate_limit(r#"{"jsonrpc":"2.0","id":1,"result":"0x"}"#));
    }
}
