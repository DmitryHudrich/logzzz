use std::sync::atomic::{AtomicBool, Ordering};
use std::{collections::HashMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use chrono::TimeZone;
use moka::future::Cache;
use serde_json::json;
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

pub const DEFAULT_BASE_URL: &str = "https://eth-mainnet.g.alchemy.com/v2";
const TRANSFER_TOPIC: &str =
    "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
const DEFAULT_KEY_COOLDOWN_SECS: u64 = 5;
const DEFAULT_LATEST_BLOCK_TTL_SECS: u64 = 15;
const DEFAULT_LOG_CHUNK_BLOCKS: u64 = 2_000;
const DEFAULT_MIN_LOG_CHUNK_BLOCKS: u64 = 16;
const DEFAULT_MAX_CONCURRENT_REQUESTS: u32 = 8;
const DEFAULT_TRACE_PAGE_SIZE: u32 = 1_000;
const DEFAULT_TRACE_MAX_PAGES: u32 = 50;
const DEFAULT_COLD_TTL_SECS: u64 = 86_400;
const DEFAULT_HOT_TTL_SECS: u64 = 15;
const DEFAULT_CONFIRMATION_DEPTH: u64 = 12;
const DEFAULT_PAGE_MAX_CAPACITY: u64 = 100_000;
const DEFAULT_HTTP_MAX_ATTEMPTS: u8 = 5;
/// Free-tier Alchemy throttles by Compute Units / second (500 CU/s with a
/// 10s burst window of 5000 CU). The rate limiter measures cost in CUs;
/// each call passes its method's CU cost. See `cu_for_method` for the
/// per-method table.
const DEFAULT_REQUESTS_PER_SECOND: f64 = 500.0;
const DEFAULT_REQUESTS_PER_SECOND_BURST: f64 = 5_000.0;
const RATE_LIMIT_BACKOFF_BASE_MS: u64 = 400;

/// CU costs per JSON-RPC method, as billed by Alchemy on the public
/// compute-unit table. We're conservative — when in doubt, pick the
/// higher of two documented numbers so we under-spend the bucket rather
/// than risk silent throttling. Batched calls multiply by the inner count.
fn cu_for_method(method: &str) -> f64 {
    match method {
        "eth_blockNumber" => 10.0,
        "eth_getCode" => 19.0,
        "eth_getBlockByNumber" => 16.0,
        "eth_call" => 26.0,
        // eth_getLogs varies — Alchemy charges per block range. 75 is the
        // standard rate without enhanced filters; if you hit logs heavy
        // workloads, tune up.
        "eth_getLogs" => 75.0,
        "trace_filter" => 75.0,
        "trace_block" => 75.0,
        // Conservative default for anything we forget — better to slightly
        // under-utilise than to silently exceed and trigger throttling.
        _ => 30.0,
    }
}

#[derive(Debug, Clone)]
pub struct AlchemyEvmConfig {
    api_keys: Vec<String>,
    base_url: String,
    key_cooldown: Duration,
    latest_block_ttl: Duration,
    max_concurrent_requests: u32,
    enable_transfers: bool,
    log_chunk_blocks: u64,
    min_log_chunk_blocks: u64,
    trace_page_size: u32,
    trace_max_pages: u32,
    cold_ttl: Duration,
    hot_ttl: Duration,
    cache_hot_tail: bool,
    confirmation_depth: u64,
    page_max_capacity: u64,
    requests_per_second: f64,
    requests_per_second_burst: f64,
    http_max_attempts: u8,
}

impl AlchemyEvmConfig {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        api_keys: Vec<String>,
        base_url: Option<String>,
        key_cooldown: Option<Duration>,
        latest_block_ttl: Option<Duration>,
        max_concurrent_requests: Option<u32>,
        enable_transfers: bool,
        log_chunk_blocks: Option<u64>,
        min_log_chunk_blocks: Option<u64>,
        trace_page_size: Option<u32>,
        trace_max_pages: Option<u32>,
        cold_ttl: Option<Duration>,
        hot_ttl: Option<Duration>,
        cache_hot_tail: Option<bool>,
        confirmation_depth: Option<u64>,
        page_max_capacity: Option<u64>,
        requests_per_second: Option<f64>,
        requests_per_second_burst: Option<f64>,
        http_max_attempts: Option<u8>,
    ) -> Self {
        let api_keys: Vec<String> = api_keys
            .into_iter()
            .filter(|k| !k.trim().is_empty())
            .collect();
        Self {
            api_keys,
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.into()),
            key_cooldown: key_cooldown
                .unwrap_or_else(|| Duration::from_secs(DEFAULT_KEY_COOLDOWN_SECS)),
            latest_block_ttl: latest_block_ttl
                .unwrap_or_else(|| Duration::from_secs(DEFAULT_LATEST_BLOCK_TTL_SECS)),
            max_concurrent_requests: max_concurrent_requests
                .unwrap_or(DEFAULT_MAX_CONCURRENT_REQUESTS)
                .max(1),
            enable_transfers,
            log_chunk_blocks: log_chunk_blocks.unwrap_or(DEFAULT_LOG_CHUNK_BLOCKS).max(1),
            min_log_chunk_blocks: min_log_chunk_blocks
                .unwrap_or(DEFAULT_MIN_LOG_CHUNK_BLOCKS)
                .max(1),
            trace_page_size: trace_page_size
                .unwrap_or(DEFAULT_TRACE_PAGE_SIZE)
                .clamp(1, 10_000),
            trace_max_pages: trace_max_pages.unwrap_or(DEFAULT_TRACE_MAX_PAGES).max(1),
            cold_ttl: cold_ttl.unwrap_or_else(|| Duration::from_secs(DEFAULT_COLD_TTL_SECS)),
            hot_ttl: hot_ttl.unwrap_or_else(|| Duration::from_secs(DEFAULT_HOT_TTL_SECS)),
            cache_hot_tail: cache_hot_tail.unwrap_or(true),
            confirmation_depth: confirmation_depth.unwrap_or(DEFAULT_CONFIRMATION_DEPTH),
            page_max_capacity: page_max_capacity.unwrap_or(DEFAULT_PAGE_MAX_CAPACITY).max(1),
            requests_per_second: requests_per_second.unwrap_or(DEFAULT_REQUESTS_PER_SECOND),
            requests_per_second_burst: requests_per_second_burst
                .unwrap_or(DEFAULT_REQUESTS_PER_SECOND_BURST),
            http_max_attempts: http_max_attempts.unwrap_or(DEFAULT_HTTP_MAX_ATTEMPTS).max(1),
        }
    }

    pub fn has_keys(&self) -> bool {
        !self.api_keys.is_empty()
    }

    /// Per-chain Alchemy endpoint override. Alchemy uses chain-specific
    /// hostnames (eth-mainnet / polygon-mainnet / base-mainnet / ...) that
    /// share the same API-key pool. Multi-chain deployments MUST override
    /// this when reusing one `alchemy:` section across chains.
    pub fn with_base_url(mut self, url: String) -> Self {
        self.base_url = url;
        self
    }
}

/// Structured cache key for the two paginated upstream endpoints
/// (`trace_filter` per address-side, `eth_getLogs` per topic-slot side).
/// Block range is part of the key so disjoint windows don't collide, and
/// `after` (trace_filter pagination cursor) keeps successive pages
/// independent. ERC-20 logs don't paginate via a cursor — each chunk
/// resolved by the bisection loop is keyed by its `(lo, hi)` window.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum PageKey {
    TraceFrom {
        address: String,
        from_block: u64,
        to_block: u64,
        after: u32,
    },
    TraceTo {
        address: String,
        from_block: u64,
        to_block: u64,
        after: u32,
    },
    LogsFrom {
        topic: String,
        from_block: u64,
        to_block: u64,
    },
    LogsTo {
        topic: String,
        from_block: u64,
        to_block: u64,
    },
}

impl PageKey {
    /// Upper block bound — used by `classify_hot` to decide whether the
    /// page can still reorg.
    fn to_block(&self) -> u64 {
        match self {
            PageKey::TraceFrom { to_block, .. }
            | PageKey::TraceTo { to_block, .. }
            | PageKey::LogsFrom { to_block, .. }
            | PageKey::LogsTo { to_block, .. } => *to_block,
        }
    }
}

type PageValue = Arc<Vec<serde_json::Value>>;

#[derive(Clone)]
pub struct AlchemyEvmSource {
    chain: ChainId,
    key_pool: KeyPool,
    base_url: String,
    enable_transfers: bool,
    log_chunk_blocks: u64,
    min_log_chunk_blocks: u64,
    trace_page_size: u32,
    trace_max_pages: u32,
    cache_hot_tail: bool,
    confirmation_depth: u64,
    http_max_attempts: u8,
    client: reqwest::Client,
    latest_block_cache: Cache<(), u64>,
    is_contract_cache: Cache<Vec<u8>, bool>,
    cold_page_cache: Cache<PageKey, PageValue>,
    hot_page_cache: Cache<PageKey, PageValue>,
    request_permits: Arc<Semaphore>,
    rate_limiter: Arc<RateLimiter>,
    /// Once a JSON-RPC `eth_getCode` batch resolves zero of N entries we
    /// flip this and stop sending batches for the rest of the process'
    /// lifetime — individual calls with rate-limit + cache still work and
    /// don't burn round-trips that demonstrably return nothing useful.
    batch_eth_get_code_disabled: Arc<AtomicBool>,
}

impl AlchemyEvmSource {
    pub fn new(chain: ChainId, client: reqwest::Client, cfg: AlchemyEvmConfig) -> Self {
        assert!(
            cfg.has_keys(),
            "AlchemyEvmSource: at least one api key required — config validation must guard this"
        );
        let key_pool = KeyPool::new(cfg.api_keys.clone(), cfg.key_cooldown);

        let latest_block_cache = Cache::builder()
            .max_capacity(1)
            .time_to_live(cfg.latest_block_ttl)
            .build();

        // Contract code is immutable in practice (post-Cancun SELFDESTRUCT
        // is a no-op for code removal); cache aggressively. 7d mirrors the
        // Etherscan source so the routed pipeline behaves consistently.
        let is_contract_cache = Cache::builder()
            .max_capacity(100_000)
            .time_to_live(Duration::from_secs(7 * 24 * 3600))
            .build();

        // Cold pages: finalized data, long TTL. Hot pages: still-reorgable
        // tail, short TTL (or disabled via cache_hot_tail). Capacity is
        // weighed by the number of rows in the page so large traces don't
        // crowd small ones out of the cache.
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

        let request_permits = Arc::new(Semaphore::new(cfg.max_concurrent_requests as usize));
        let rate_limiter = Arc::new(RateLimiter::new(
            cfg.requests_per_second,
            cfg.requests_per_second_burst,
        ));

        tracing::info!(
            base_url = %cfg.base_url,
            api_keys = key_pool.len(),
            key_cooldown_secs = cfg.key_cooldown.as_secs(),
            latest_block_ttl_secs = cfg.latest_block_ttl.as_secs(),
            max_concurrent_requests = cfg.max_concurrent_requests,
            enable_transfers = cfg.enable_transfers,
            log_chunk_blocks = cfg.log_chunk_blocks,
            min_log_chunk_blocks = cfg.min_log_chunk_blocks,
            trace_page_size = cfg.trace_page_size,
            cold_ttl_secs = cfg.cold_ttl.as_secs(),
            hot_ttl_secs = cfg.hot_ttl.as_secs(),
            cache_hot_tail = cfg.cache_hot_tail,
            confirmation_depth = cfg.confirmation_depth,
            page_max_capacity = cfg.page_max_capacity,
            requests_per_second = cfg.requests_per_second,
            requests_per_second_burst = cfg.requests_per_second_burst,
            http_max_attempts = cfg.http_max_attempts,
            chain_id = chain.value(),
            "Alchemy EVM source initialized"
        );

        Self {
            chain,
            key_pool,
            base_url: cfg.base_url,
            enable_transfers: cfg.enable_transfers,
            log_chunk_blocks: cfg.log_chunk_blocks,
            min_log_chunk_blocks: cfg.min_log_chunk_blocks,
            trace_page_size: cfg.trace_page_size,
            trace_max_pages: cfg.trace_max_pages,
            cache_hot_tail: cfg.cache_hot_tail,
            confirmation_depth: cfg.confirmation_depth,
            http_max_attempts: cfg.http_max_attempts,
            client,
            latest_block_cache,
            is_contract_cache,
            cold_page_cache,
            hot_page_cache,
            request_permits,
            rate_limiter,
            batch_eth_get_code_disabled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Single point of contact with Alchemy: builds the per-key URL, sends
    /// the JSON-RPC envelope, parses the response. Two layers of throttle:
    /// * token-bucket rate limiter (`requests_per_second`) — proactive;
    /// * per-key cooldown + `pick_or_wait` — reactive on observed 429/RPC
    ///   rate-limit, with REAL retries (loop continues after sleep instead
    ///   of bailing the first time the only key cools).
    async fn jsonrpc_call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> DomainResult<serde_json::Value> {
        let mut last_err = String::new();
        let cost = cu_for_method(method);
        for attempt in 0..self.http_max_attempts {
            // Layer 1: CU-priced token bucket. Each single-method call
            // pays its method's CU cost so the bucket reflects Alchemy's
            // actual billing dimension, not raw request count.
            self.rate_limiter.acquire(cost).await;

            // Layer 2: live key or wait for soonest cooldown to lapse.
            let api_key = match self.key_pool.pick_or_wait() {
                Ok(k) => k,
                Err(wait) => {
                    tracing::warn!(
                        method,
                        attempt,
                        wait_ms = wait.as_millis() as u64,
                        "alchemy: all keys cooled, waiting before retry"
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

            let url = format!("{}/{}", self.base_url.trim_end_matches('/'), api_key);
            let body = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": method,
                "params": params,
            });

            let permit = match self.request_permits.clone().acquire_owned().await {
                Ok(p) => p,
                Err(e) => {
                    return Err(DomainError::InsufficientData(format!(
                        "alchemy: semaphore closed: {e}"
                    )));
                }
            };

            let resp = match self.client.post(&url).json(&body).send().await {
                Ok(r) => r,
                Err(e) => {
                    drop(permit);
                    tracing::warn!(method, attempt, error = %e, "alchemy http failed");
                    last_err = e.to_string();
                    continue;
                }
            };
            let status = resp.status();
            let text = match resp.text().await {
                Ok(t) => t,
                Err(e) => {
                    drop(permit);
                    tracing::warn!(method, attempt, error = %e, "alchemy body read failed");
                    last_err = e.to_string();
                    continue;
                }
            };
            drop(permit);

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                tracing::warn!(method, attempt, "alchemy HTTP 429, cooling key");
                self.key_pool.cool(&api_key);
                last_err = format!("http {status}");
                continue;
            }
            if status.is_server_error() {
                tracing::warn!(method, attempt, status = status.as_u16(), "alchemy 5xx, retrying");
                last_err = format!("http {status}");
                continue;
            }
            if !status.is_success() {
                return Err(DomainError::InsufficientData(format!(
                    "alchemy {method}: http {status}: {}",
                    text.chars().take(200).collect::<String>()
                )));
            }

            let v: serde_json::Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    return Err(DomainError::InsufficientData(format!(
                        "alchemy {method} parse: {e}: {}",
                        text.chars().take(200).collect::<String>()
                    )));
                }
            };

            if let Some(err) = v.get("error") {
                let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
                let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("");
                // Standard JSON-RPC rate-limit codes Alchemy uses.
                if code == 429 || code == -32007 || msg.to_ascii_lowercase().contains("rate limit")
                {
                    tracing::warn!(method, attempt, code, msg, "alchemy rpc rate-limited, cooling key");
                    self.key_pool.cool(&api_key);
                    last_err = msg.to_string();
                    continue;
                }
                return Err(DomainError::InsufficientData(format!(
                    "alchemy {method} rpc error {code}: {msg}"
                )));
            }

            return Ok(v.get("result").cloned().unwrap_or(serde_json::Value::Null));
        }
        Err(DomainError::RateLimited(format!(
            "alchemy {method}: after {} attempts: {last_err}",
            self.http_max_attempts
        )))
    }

    async fn eth_block_number(&self) -> DomainResult<u64> {
        if let Some(h) = self.latest_block_cache.get(&()).await {
            return Ok(h);
        }
        let res = self.jsonrpc_call("eth_blockNumber", json!([])).await?;
        let hex = res
            .as_str()
            .ok_or_else(|| DomainError::InsufficientData("alchemy eth_blockNumber: not a hex string".into()))?;
        let h = parse_hex_u64(hex)
            .map_err(|e| DomainError::InsufficientData(format!("alchemy eth_blockNumber: {e}")))?;
        self.latest_block_cache.insert((), h).await;
        Ok(h)
    }

    /// Send a JSON-RPC batch request (array of envelopes) — Alchemy
    /// accepts up to several hundred per call. Each envelope has its own
    /// `id`; we preserve them so the caller can stitch results by index.
    /// Throttling and retries mirror `jsonrpc_call`: one token from the
    /// rate limiter for the entire batch, key rotation on rate-limit,
    /// `http_max_attempts` retries.
    async fn jsonrpc_batch_call(
        &self,
        batch: &[serde_json::Value],
    ) -> DomainResult<Vec<serde_json::Value>> {
        let mut last_err = String::new();
        // A JSON-RPC batch costs the SUM of inner methods' CUs — Alchemy
        // bills each inner call, even when wrapped in one HTTP envelope.
        // Inspect each request's `method` to price correctly; default to
        // a conservative per-call CU when not detectable.
        let cost: f64 = batch
            .iter()
            .map(|req| {
                req.get("method")
                    .and_then(|v| v.as_str())
                    .map(cu_for_method)
                    .unwrap_or(30.0)
            })
            .sum();
        for attempt in 0..self.http_max_attempts {
            self.rate_limiter.acquire(cost).await;
            let api_key = match self.key_pool.pick_or_wait() {
                Ok(k) => k,
                Err(wait) => {
                    tracing::warn!(
                        attempt,
                        wait_ms = wait.as_millis() as u64,
                        batch_cu = cost,
                        "alchemy batch: all keys cooled, waiting before retry"
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

            let url = format!("{}/{}", self.base_url.trim_end_matches('/'), api_key);
            let permit = match self.request_permits.clone().acquire_owned().await {
                Ok(p) => p,
                Err(e) => {
                    return Err(DomainError::InsufficientData(format!(
                        "alchemy batch: semaphore closed: {e}"
                    )));
                }
            };

            let resp = match self.client.post(&url).json(&batch).send().await {
                Ok(r) => r,
                Err(e) => {
                    drop(permit);
                    tracing::warn!(attempt, error = %e, "alchemy batch http failed");
                    last_err = e.to_string();
                    continue;
                }
            };
            let status = resp.status();
            let text = match resp.text().await {
                Ok(t) => t,
                Err(e) => {
                    drop(permit);
                    tracing::warn!(attempt, error = %e, "alchemy batch body read failed");
                    last_err = e.to_string();
                    continue;
                }
            };
            drop(permit);

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                tracing::warn!(attempt, "alchemy batch HTTP 429, cooling key");
                self.key_pool.cool(&api_key);
                last_err = format!("http {status}");
                continue;
            }
            if status.is_server_error() {
                tracing::warn!(attempt, status = status.as_u16(), "alchemy batch 5xx, retrying");
                last_err = format!("http {status}");
                continue;
            }
            if !status.is_success() {
                return Err(DomainError::InsufficientData(format!(
                    "alchemy batch: http {status}: {}",
                    text.chars().take(200).collect::<String>()
                )));
            }

            let v: serde_json::Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    return Err(DomainError::InsufficientData(format!(
                        "alchemy batch parse: {e}: {}",
                        text.chars().take(200).collect::<String>()
                    )));
                }
            };
            let arr = match v.as_array() {
                Some(a) => a.clone(),
                None => {
                    return Err(DomainError::InsufficientData(format!(
                        "alchemy batch: expected array, got: {}",
                        text.chars().take(200).collect::<String>()
                    )));
                }
            };
            return Ok(arr);
        }
        Err(DomainError::RateLimited(format!(
            "alchemy batch: after {} attempts: {last_err}",
            self.http_max_attempts
        )))
    }

    /// Bulk `eth_getCode` via JSON-RPC batch with per-entry retry for
    /// inner 429s. Alchemy explicitly documents that batched entries may
    /// individually exceed CU/s and return `{"code": 429, "message": "...
    /// you can safely ignore this message"}` — they expect the client to
    /// retry those entries. We do exactly that: each pass sends only the
    /// addresses still unresolved (or specifically inner-429'd), with
    /// fresh batch-local IDs, and the rate limiter slows us between
    /// passes so the retry actually has new budget.
    ///
    /// Capped at `BATCH_INNER_MAX_ATTEMPTS` so a persistently-broken
    /// downstream (genuinely malformed responses, RPC errors that aren't
    /// 429) eventually settles to "soft-unknown" instead of looping.
    async fn eth_get_code_batch(
        &self,
        addresses_hex: &[String],
    ) -> DomainResult<Vec<Option<bool>>> {
        const BATCH_INNER_MAX_ATTEMPTS: u8 = 3;

        if addresses_hex.is_empty() {
            return Ok(Vec::new());
        }
        let total = addresses_hex.len();
        let mut out: Vec<Option<bool>> = vec![None; total];
        // Map from "batch-local id we sent" → "original output slot".
        // Refreshed every attempt because we re-id only the unresolved.
        let mut remaining: Vec<usize> = (0..total).collect();

        let mut last_response_len: usize = 0;
        let mut last_sample: String = String::new();

        for attempt in 0..BATCH_INNER_MAX_ATTEMPTS {
            if remaining.is_empty() {
                break;
            }
            let batch: Vec<serde_json::Value> = remaining
                .iter()
                .enumerate()
                .map(|(local_id, &orig_slot)| {
                    json!({
                        "jsonrpc": "2.0",
                        "id": local_id,
                        "method": "eth_getCode",
                        "params": [&addresses_hex[orig_slot], "latest"],
                    })
                })
                .collect();

            let arr = self.jsonrpc_batch_call(&batch).await?;
            last_response_len = arr.len();
            if last_sample.is_empty() {
                if let Some(first) = arr.first() {
                    last_sample = first.to_string();
                }
            }

            let mut next_remaining: Vec<usize> = Vec::new();
            let mut seen_local: std::collections::HashSet<usize> =
                std::collections::HashSet::new();
            let mut inner_429 = 0usize;
            let mut errors = 0usize;
            let mut null_results = 0usize;
            let mut id_out_of_range = 0usize;
            let mut id_unparseable = 0usize;
            let mut malformed = 0usize;
            let mut empty_code = 0usize;
            let mut with_code = 0usize;

            for entry in &arr {
                let id_u64 = entry
                    .get("id")
                    .and_then(|v| v.as_u64())
                    .or_else(|| {
                        entry
                            .get("id")
                            .and_then(|v| v.as_i64())
                            .and_then(|n| u64::try_from(n).ok())
                    })
                    .or_else(|| {
                        entry
                            .get("id")
                            .and_then(|v| v.as_str())
                            .and_then(|s| s.parse().ok())
                    });
                let Some(id_u64) = id_u64 else {
                    id_unparseable += 1;
                    continue;
                };
                let local_id = id_u64 as usize;
                if local_id >= remaining.len() {
                    id_out_of_range += 1;
                    continue;
                }
                seen_local.insert(local_id);
                let orig_slot = remaining[local_id];

                if let Some(err) = entry.get("error") {
                    let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
                    if code == 429 {
                        inner_429 += 1;
                        next_remaining.push(orig_slot);
                        if inner_429 <= 3 {
                            tracing::debug!(
                                local_id,
                                orig_slot,
                                attempt,
                                "alchemy batch entry inner 429 — will retry"
                            );
                        }
                    } else {
                        errors += 1;
                        if errors <= 3 {
                            tracing::debug!(
                                local_id,
                                orig_slot,
                                ?err,
                                "alchemy batch entry returned non-429 RPC error — leaving Unknown"
                            );
                        }
                    }
                    continue;
                }

                let Some(s) = entry.get("result").and_then(|v| v.as_str()) else {
                    null_results += 1;
                    continue;
                };
                let stripped = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                    Some(s) => s,
                    None => {
                        malformed += 1;
                        continue;
                    }
                };
                if !stripped.chars().all(|c| c.is_ascii_hexdigit()) || stripped.len() % 2 != 0 {
                    malformed += 1;
                    continue;
                }
                if stripped.is_empty() {
                    empty_code += 1;
                    out[orig_slot] = Some(false);
                } else {
                    with_code += 1;
                    out[orig_slot] = Some(true);
                }
            }

            // Any entry in `remaining` that we never saw a response for —
            // Alchemy returned a short array. Treat as needs-retry too.
            for (local_id, &orig_slot) in remaining.iter().enumerate() {
                if !seen_local.contains(&local_id) && out[orig_slot].is_none() {
                    next_remaining.push(orig_slot);
                }
            }
            next_remaining.sort();
            next_remaining.dedup();

            if inner_429 > 0 || !next_remaining.is_empty() {
                tracing::debug!(
                    attempt,
                    sent = remaining.len(),
                    response_len = arr.len(),
                    ok = empty_code + with_code,
                    inner_429,
                    errors,
                    null_results,
                    id_out_of_range,
                    id_unparseable,
                    malformed,
                    next_attempt_size = next_remaining.len(),
                    "alchemy batch attempt finished"
                );
            }

            // Sanity check: if NOTHING resolved AND nothing was 429, the
            // shape is wrong — break early so the caller's sticky fallback
            // kicks in instead of looping at full cost.
            let resolved_this_pass = empty_code + with_code;
            if attempt == 0
                && resolved_this_pass == 0
                && inner_429 == 0
                && !remaining.is_empty()
            {
                tracing::warn!(
                    batch_size = remaining.len(),
                    response_len = arr.len(),
                    errors,
                    null_results,
                    id_out_of_range,
                    id_unparseable,
                    malformed,
                    sample_entry = %last_sample.chars().take(300).collect::<String>(),
                    "alchemy batch: 0 resolved AND 0 inner-429 on first pass — likely a shape issue, breaking out"
                );
                break;
            }

            remaining = next_remaining;
        }

        let unresolved = out.iter().filter(|x| x.is_none()).count();
        if unresolved == total && total > 0 {
            tracing::warn!(
                batch_size = total,
                last_response_len,
                sample_entry = %last_sample.chars().take(300).collect::<String>(),
                "alchemy batch: ALL entries still unresolved after retries"
            );
        }

        Ok(out)
    }

    async fn eth_get_code(&self, address_hex: &str) -> DomainResult<Option<bool>> {
        let res = self
            .jsonrpc_call("eth_getCode", json!([address_hex, "latest"]))
            .await?;
        let s = match res.as_str() {
            Some(s) => s,
            None => return Ok(None),
        };
        // Mirror Etherscan's parse_get_code semantics: `0x` → EOA, `0x...`
        // (non-empty even-hex) → contract, anything else → soft-unknown.
        let stripped = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            Some(s) => s,
            None => return Ok(None),
        };
        if !stripped.chars().all(|c| c.is_ascii_hexdigit()) || stripped.len() % 2 != 0 {
            return Ok(None);
        }
        Ok(Some(!stripped.is_empty()))
    }

    /// A page is "hot" if its upper bound sits inside the unfinalized tail
    /// `latest - confirmation_depth`. Anything older is finalized and safe
    /// to cache for `cold_ttl`.
    async fn classify_hot(&self, key: &PageKey) -> bool {
        let latest = match self.eth_block_number().await {
            Ok(h) => h,
            Err(_) => return true,
        };
        let cutoff = latest.saturating_sub(self.confirmation_depth);
        key.to_block() > cutoff
    }

    async fn lookup_page(&self, key: &PageKey) -> Option<PageValue> {
        if let Some(v) = self.cold_page_cache.get(key).await {
            return Some(v);
        }
        self.hot_page_cache.get(key).await
    }

    async fn insert_page(&self, key: PageKey, value: PageValue, is_hot: bool) {
        if is_hot {
            if self.cache_hot_tail {
                self.hot_page_cache.insert(key, value).await;
            }
            // Hot pages never enter the cold cache — they may reorg.
        } else {
            self.cold_page_cache.insert(key, value).await;
        }
    }

    /// Fetch one `trace_filter` page, consulting the page cache first.
    /// Cache hits are returned as-is; misses fall through to the network
    /// and store under the right (cold/hot) cache based on classify_hot.
    async fn trace_filter_page(
        &self,
        key: PageKey,
    ) -> DomainResult<PageValue> {
        if let Some(v) = self.lookup_page(&key).await {
            tracing::debug!(?key, "alchemy trace_filter page cache hit");
            return Ok(v);
        }

        let (address, filter_field, from_block, to_block, after) = match &key {
            PageKey::TraceFrom { address, from_block, to_block, after } => {
                (address.clone(), "fromAddress", *from_block, *to_block, *after)
            }
            PageKey::TraceTo { address, from_block, to_block, after } => {
                (address.clone(), "toAddress", *from_block, *to_block, *after)
            }
            _ => unreachable!("trace_filter_page called with non-trace key"),
        };
        let mut filter = serde_json::Map::new();
        filter.insert("fromBlock".into(), json!(format!("0x{from_block:x}")));
        filter.insert("toBlock".into(), json!(format!("0x{to_block:x}")));
        filter.insert(filter_field.into(), json!([address]));
        filter.insert("after".into(), json!(after));
        filter.insert("count".into(), json!(self.trace_page_size));

        let res = self.jsonrpc_call("trace_filter", json!([filter])).await?;
        let rows = res.as_array().cloned().unwrap_or_default();
        let arc = Arc::new(rows);

        let is_hot = self.classify_hot(&key).await;
        self.insert_page(key, Arc::clone(&arc), is_hot).await;
        Ok(arc)
    }

    /// Paginate `trace_filter` results until empty page, `count` cap, or
    /// `max_traces` budget. Each page is cached individually so repeat BFS
    /// passes hit the cache before issuing HTTP.
    async fn trace_filter_by_address(
        &self,
        from_block: u64,
        to_block: u64,
        filter_key: &str,
        address_hex: &str,
        max_traces: usize,
    ) -> DomainResult<Vec<serde_json::Value>> {
        let mut out: Vec<serde_json::Value> = Vec::new();
        let mut after: u32 = 0;
        for page in 0..self.trace_max_pages {
            let key = match filter_key {
                "fromAddress" => PageKey::TraceFrom {
                    address: address_hex.to_string(),
                    from_block,
                    to_block,
                    after,
                },
                "toAddress" => PageKey::TraceTo {
                    address: address_hex.to_string(),
                    from_block,
                    to_block,
                    after,
                },
                other => {
                    return Err(DomainError::InsufficientData(format!(
                        "alchemy: unknown trace filter key '{other}'"
                    )));
                }
            };
            let rows = self.trace_filter_page(key).await?;
            let n = rows.len();
            out.extend(rows.iter().cloned());
            tracing::debug!(
                filter_key,
                address_hex,
                page,
                page_len = n,
                total = out.len(),
                "alchemy trace_filter paginated"
            );
            if n < self.trace_page_size as usize || out.len() >= max_traces {
                break;
            }
            after = after.saturating_add(self.trace_page_size);
        }
        Ok(out)
    }

    /// Issue one `eth_getLogs` chunk, consulting the page cache first.
    /// Returns the network/cache rows wrapped in Arc; if the upstream
    /// flagged the window as too large, propagates `InsufficientData` so
    /// the bisection loop above can subdivide and retry.
    async fn get_logs_chunk(
        &self,
        lo: u64,
        hi: u64,
        topic_from: Option<&str>,
        topic_to: Option<&str>,
    ) -> DomainResult<PageValue> {
        let key = match (topic_from, topic_to) {
            (Some(t), None) => PageKey::LogsFrom {
                topic: t.to_string(),
                from_block: lo,
                to_block: hi,
            },
            (None, Some(t)) => PageKey::LogsTo {
                topic: t.to_string(),
                from_block: lo,
                to_block: hi,
            },
            // Caller always sends exactly one side; reject otherwise so we
            // don't quietly cache mixed-shape entries.
            _ => {
                return Err(DomainError::InsufficientData(
                    "alchemy get_logs_chunk: exactly one of topic_from/topic_to is required".into(),
                ));
            }
        };

        if let Some(v) = self.lookup_page(&key).await {
            tracing::debug!(?key, "alchemy eth_getLogs page cache hit");
            return Ok(v);
        }

        let topics = build_transfer_topics(topic_from, topic_to);
        let filter = json!({
            "fromBlock": format!("0x{lo:x}"),
            "toBlock": format!("0x{hi:x}"),
            "topics": topics,
        });
        let res = self.jsonrpc_call("eth_getLogs", json!([filter])).await?;
        let rows = res.as_array().cloned().unwrap_or_default();
        let arc = Arc::new(rows);

        let is_hot = self.classify_hot(&key).await;
        self.insert_page(key, Arc::clone(&arc), is_hot).await;
        Ok(arc)
    }

    /// Recursive `eth_getLogs` over `(from, to)`, bisecting the block range
    /// on response-size errors. `topic_from` / `topic_to` are padded 32-byte
    /// hex addresses placed into topic1 / topic2 respectively; passing one
    /// implements a from-side or to-side filter (Alchemy supports OR within
    /// a topic slot but not across — hence two separate calls upstream).
    async fn get_logs_chunked(
        &self,
        from_block: u64,
        to_block: u64,
        topic_from: Option<&str>,
        topic_to: Option<&str>,
        max_logs: usize,
    ) -> DomainResult<Vec<serde_json::Value>> {
        let mut stack: Vec<(u64, u64)> = vec![(from_block, to_block)];
        let mut out: Vec<serde_json::Value> = Vec::new();
        while let Some((lo, hi)) = stack.pop() {
            if out.len() >= max_logs {
                break;
            }
            // Optimistically respect the configured chunk size; on response
            // overflow we'll bisect via the error branch below.
            let actual_hi = hi.min(lo.saturating_add(self.log_chunk_blocks.saturating_sub(1)));
            let queue_rest = actual_hi < hi;

            match self.get_logs_chunk(lo, actual_hi, topic_from, topic_to).await {
                Ok(rows) => {
                    let n = rows.len();
                    out.extend(rows.iter().cloned());
                    tracing::debug!(
                        from = lo,
                        to = actual_hi,
                        page_len = n,
                        total = out.len(),
                        "alchemy eth_getLogs chunk"
                    );
                    if queue_rest {
                        stack.push((actual_hi + 1, hi));
                    }
                }
                Err(DomainError::InsufficientData(msg)) if is_log_response_too_large(&msg) => {
                    let span = actual_hi.saturating_sub(lo);
                    if span < self.min_log_chunk_blocks {
                        return Err(DomainError::InsufficientData(format!(
                            "alchemy eth_getLogs: cannot subdivide below {} blocks: {msg}",
                            self.min_log_chunk_blocks
                        )));
                    }
                    let mid = lo + span / 2;
                    tracing::warn!(
                        from = lo,
                        to = actual_hi,
                        mid,
                        "alchemy eth_getLogs response too large, bisecting"
                    );
                    if queue_rest {
                        stack.push((actual_hi + 1, hi));
                    }
                    stack.push((mid + 1, actual_hi));
                    stack.push((lo, mid));
                }
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }
}

#[async_trait]
impl ChainSource for AlchemyEvmSource {
    fn chain_id(&self) -> ChainId {
        self.chain
    }

    async fn latest_block(&self) -> DomainResult<BlockRef> {
        let h = self.eth_block_number().await?;
        Ok(BlockRef::new(self.chain, h, [0u8; 32]))
    }

    async fn fetch_block(&self, height: u64) -> DomainResult<NormalizedBlock> {
        // Native value transfers come from the block body; ERC-20 from
        // Transfer-topic logs filtered to this single block. Fan both
        // requests out in parallel — they're independent.
        let block_call = self.jsonrpc_call(
            "eth_getBlockByNumber",
            json!([format!("0x{height:x}"), true]),
        );
        let logs_filter = json!({
            "fromBlock": format!("0x{height:x}"),
            "toBlock":   format!("0x{height:x}"),
            "topics":    [TRANSFER_TOPIC, null, null],
        });
        let logs_call = self.jsonrpc_call("eth_getLogs", json!([logs_filter]));
        let (block_v, logs_v) = tokio::try_join!(block_call, logs_call)?;
        let log_rows = logs_v.as_array().cloned().unwrap_or_default();

        // Block envelope: hash + timestamp + the transaction list.
        let block_hash_s = block_v
            .get("hash")
            .and_then(|v| v.as_str())
            .ok_or_else(|| DomainError::InsufficientData("alchemy fetch_block: missing block hash".into()))?;
        let ts_hex = block_v
            .get("timestamp")
            .and_then(|v| v.as_str())
            .ok_or_else(|| DomainError::InsufficientData("alchemy fetch_block: missing timestamp".into()))?;
        let block_hash = parse_hash32(block_hash_s)
            .map_err(|e| DomainError::InsufficientData(format!("alchemy fetch_block: block hash: {e}")))?;
        let ts_secs = parse_hex_u64(ts_hex)
            .map_err(|e| DomainError::InsufficientData(format!("alchemy fetch_block: timestamp: {e}")))?
            as i64;
        let timestamp = chrono::Utc
            .timestamp_opt(ts_secs, 0)
            .single()
            .ok_or_else(|| DomainError::InsufficientData(format!("alchemy fetch_block: bad timestamp {ts_secs}")))?;

        let block_ref = BlockRef::new(self.chain, height, block_hash);
        let mut block_ts: HashMap<u64, chrono::DateTime<chrono::Utc>> = HashMap::new();
        block_ts.insert(height, timestamp);
        let mut by_tx: HashMap<[u8; 32], u32> = HashMap::new();
        let mut transfers: Vec<Transfer> = Vec::new();

        // Native value transfers from the body. The outer tx claims idx=0
        // per the existing convention (matches Etherscan source); idx 1+
        // is reserved for inner traces and ERC-20 events in the same tx.
        if let Some(txs) = block_v.get("transactions").and_then(|v| v.as_array()) {
            for raw in txs {
                match map_native_tx_to_transfer(self.chain, raw, height, block_hash, timestamp, &mut by_tx) {
                    Ok(Some(t)) => transfers.push(t),
                    Ok(None) => {}
                    Err(e) => tracing::warn!(error = %e, "alchemy fetch_block: skip malformed tx"),
                }
            }
        }

        // ERC-20 Transfer events in the same block. Each log claims the
        // next free idx within its tx so we never collide on the PK.
        for raw in log_rows {
            match map_log_to_transfer(self.chain, &raw, &block_ts, &mut by_tx) {
                Ok(Some(t)) => transfers.push(t),
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, "alchemy fetch_block: skip malformed log"),
            }
        }

        transfers.sort_by_key(|t| t.id().index());
        Ok(NormalizedBlock::new(block_ref, timestamp, transfers))
    }

    async fn transfers_for_address(
        &self,
        addr: &Address,
        range: BlockRange,
        max_transfers: usize,
    ) -> DomainResult<Vec<Transfer>> {
        if !self.enable_transfers {
            return Err(DomainError::InsufficientData(
                "alchemy: transfers disabled (set alchemy.enable_transfers=true to enable)".into(),
            ));
        }
        if addr.chain() != self.chain {
            return Err(DomainError::InsufficientData(format!(
                "alchemy source (chain={}) called with foreign address chain: {}",
                self.chain,
                addr.chain()
            )));
        }
        let address_hex = format!("0x{}", hex::encode(addr.bytes()));
        let padded = format!("0x{:0>64}", hex::encode(addr.bytes()));

        let from_block = range.from_height();
        let to_block = if range.to_height() == u64::MAX {
            self.eth_block_number().await?
        } else {
            range.to_height()
        };

        tracing::info!(
            address = %address_hex,
            from_block,
            to_block,
            max_transfers,
            "alchemy: fetching ETH transfers"
        );

        let (traces_from, traces_to, logs_from, logs_to) = tokio::try_join!(
            self.trace_filter_by_address(from_block, to_block, "fromAddress", &address_hex, max_transfers),
            self.trace_filter_by_address(from_block, to_block, "toAddress", &address_hex, max_transfers),
            self.get_logs_chunked(from_block, to_block, Some(&padded), None, max_transfers),
            self.get_logs_chunked(from_block, to_block, None, Some(&padded), max_transfers),
        )?;

        // Map block timestamps from blockNumber → we'd normally need a
        // per-block lookup but trace_filter returns blockNumber and logs do
        // too; we cache timestamps in-flight via a per-call map populated
        // lazily through eth_getBlockByNumber. This adds at most one call
        // per distinct block touched in the result — acceptable in MVP.
        let mut block_ts: HashMap<u64, chrono::DateTime<chrono::Utc>> = HashMap::new();

        // Native: merge traces dedupe by (tx_hash, traceAddress).
        let mut traces: HashMap<(Vec<u8>, String), serde_json::Value> = HashMap::new();
        for raw in traces_from.into_iter().chain(traces_to.into_iter()) {
            let tx_hash_s = match raw.get("transactionHash").and_then(|v| v.as_str()) {
                Some(s) => s,
                None => continue,
            };
            let tx_hash_bytes = match hex::decode(tx_hash_s.trim_start_matches("0x")) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let trace_addr = raw
                .get("traceAddress")
                .map(|v| v.to_string())
                .unwrap_or_default();
            traces.entry((tx_hash_bytes, trace_addr)).or_insert(raw);
        }

        // ERC-20 logs: dedupe by (tx_hash, logIndex).
        let mut logs: HashMap<(Vec<u8>, u64), serde_json::Value> = HashMap::new();
        for raw in logs_from.into_iter().chain(logs_to.into_iter()) {
            let tx_hash_s = match raw.get("transactionHash").and_then(|v| v.as_str()) {
                Some(s) => s,
                None => continue,
            };
            let tx_hash_bytes = match hex::decode(tx_hash_s.trim_start_matches("0x")) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let log_idx_s = raw.get("logIndex").and_then(|v| v.as_str()).unwrap_or("0x0");
            let log_idx = parse_hex_u64(log_idx_s).unwrap_or(0);
            logs.entry((tx_hash_bytes, log_idx)).or_insert(raw);
        }

        let unique_blocks: std::collections::HashSet<u64> = traces
            .values()
            .filter_map(|t| t.get("blockNumber").and_then(|v| v.as_u64()).or_else(|| {
                t.get("blockNumber").and_then(|v| v.as_str()).and_then(|s| parse_hex_u64(s).ok())
            }))
            .chain(logs.values().filter_map(|l| {
                l.get("blockNumber").and_then(|v| v.as_str()).and_then(|s| parse_hex_u64(s).ok())
            }))
            .collect();

        for height in unique_blocks {
            if let Ok(ts) = self.block_timestamp(height).await {
                block_ts.insert(height, ts);
            }
        }

        let mut by_tx: HashMap<[u8; 32], u32> = HashMap::new();
        let mut out: Vec<Transfer> = Vec::with_capacity(traces.len() + logs.len());

        for (_, raw) in traces {
            match map_trace_to_transfer(self.chain, &raw, &block_ts, &mut by_tx) {
                Ok(Some(t)) => out.push(t),
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, "alchemy: skip malformed trace"),
            }
        }
        for (_, raw) in logs {
            match map_log_to_transfer(self.chain, &raw, &block_ts, &mut by_tx) {
                Ok(Some(t)) => out.push(t),
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, "alchemy: skip malformed log"),
            }
        }

        // Stable ordering by (block_height, idx) so downstream BFS state is
        // deterministic across runs.
        out.sort_by_key(|t| (t.block().height(), t.id().index()));
        if out.len() > max_transfers {
            out.truncate(max_transfers);
        }

        tracing::info!(
            address = %address_hex,
            total = out.len(),
            "alchemy: transfers fetched"
        );
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
        match self.eth_get_code(&address_hex).await {
            Ok(Some(v)) => {
                self.is_contract_cache.insert(bytes, v).await;
                Ok(Some(v))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Real bulk path: one POST with a JSON array of `eth_getCode` calls
    /// keyed by their array index. We skip cached entries entirely (no
    /// token consumed), batch the rest into a single round-trip, then
    /// stitch results back into the input order. With ~100 addresses per
    /// batch this turns 100 × RTT into 1 × RTT.
    ///
    /// Defensive: if a batch returns Ok with zero resolved entries (either
    /// Alchemy doesn't support batches on this plan, the response shape
    /// changed, or the request was silently malformed), we flip a sticky
    /// flag and fall back to parallel single-shot calls. So classification
    /// keeps working even when batching is broken — just at single-call
    /// throughput.
    async fn is_contract_batch(
        &self,
        addrs: &[Address],
    ) -> DomainResult<Vec<Option<bool>>> {
        if addrs.is_empty() {
            return Ok(Vec::new());
        }
        // Cached entries shortcut.
        let mut out: Vec<Option<bool>> = vec![None; addrs.len()];
        let mut needs_fetch_idx: Vec<usize> = Vec::with_capacity(addrs.len());
        let mut needs_fetch_addr: Vec<String> = Vec::with_capacity(addrs.len());
        for (i, addr) in addrs.iter().enumerate() {
            if addr.chain() != self.chain {
                // Stay consistent with the single-shot path: foreign chains
                // get Ok(None) (unknown), not an error.
                continue;
            }
            let bytes = addr.bytes().to_vec();
            if let Some(cached) = self.is_contract_cache.get(&bytes).await {
                out[i] = Some(cached);
                continue;
            }
            needs_fetch_idx.push(i);
            needs_fetch_addr.push(format!("0x{}", hex::encode(&bytes)));
        }
        if needs_fetch_idx.is_empty() {
            return Ok(out);
        }

        // Sticky disable — once we've proven the batch endpoint is busted
        // on this account/plan, skip it for the rest of the process.
        if self.batch_eth_get_code_disabled.load(Ordering::Relaxed) {
            return self.fan_out_individual(addrs, out, &needs_fetch_idx).await;
        }

        let results = self.eth_get_code_batch(&needs_fetch_addr).await?;
        let resolved = results.iter().filter(|r| r.is_some()).count();
        // Sticky fallback only fires when the batch path produced
        // *literally nothing* across ≥4 entries — that's a shape bug, not
        // transient throttling (inner 429s are now retried within
        // eth_get_code_batch and surface here as resolved entries). Be
        // strict so we don't disable batching the first time CU/s spikes.
        if resolved == 0 && results.len() >= 4 {
            self.batch_eth_get_code_disabled.store(true, Ordering::Relaxed);
            tracing::warn!(
                batch_size = results.len(),
                "alchemy batch resolved 0 entries even after inner retries — disabling batched eth_getCode for this process, falling back to single-shot"
            );
            return self.fan_out_individual(addrs, out, &needs_fetch_idx).await;
        }

        for (slot, res) in needs_fetch_idx.iter().zip(results.into_iter()) {
            if let Some(v) = res {
                let bytes = addrs[*slot].bytes().to_vec();
                self.is_contract_cache.insert(bytes, v).await;
                out[*slot] = Some(v);
            }
        }
        Ok(out)
    }
}

impl AlchemyEvmSource {
    /// Parallel single-shot fallback when the batch path can't be trusted.
    /// `out` carries the cache hits we already resolved; we only fan out
    /// for `needs_fetch_idx` entries so we don't re-fetch cached ones.
    async fn fan_out_individual(
        &self,
        addrs: &[Address],
        mut out: Vec<Option<bool>>,
        needs_fetch_idx: &[usize],
    ) -> DomainResult<Vec<Option<bool>>> {
        use futures::future::join_all;
        let futs = needs_fetch_idx.iter().map(|i| {
            let addr = &addrs[*i];
            let addr_hex = format!("0x{}", hex::encode(addr.bytes()));
            async move { (*i, self.eth_get_code(&addr_hex).await) }
        });
        let results = join_all(futs).await;
        for (slot, res) in results {
            match res {
                Ok(Some(v)) => {
                    let bytes = addrs[slot].bytes().to_vec();
                    self.is_contract_cache.insert(bytes, v).await;
                    out[slot] = Some(v);
                }
                Ok(None) => {}
                Err(_) => {} // soft-unknown; do not sink the whole batch
            }
        }
        Ok(out)
    }
}

impl AlchemyEvmSource {
    async fn block_timestamp(&self, height: u64) -> DomainResult<chrono::DateTime<chrono::Utc>> {
        let res = self
            .jsonrpc_call(
                "eth_getBlockByNumber",
                json!([format!("0x{:x}", height), false]),
            )
            .await?;
        let ts_hex = res
            .get("timestamp")
            .and_then(|v| v.as_str())
            .ok_or_else(|| DomainError::InsufficientData("alchemy: block missing timestamp".into()))?;
        let ts_secs = parse_hex_u64(ts_hex)
            .map_err(|e| DomainError::InsufficientData(format!("alchemy block timestamp: {e}")))?
            as i64;
        chrono::Utc
            .timestamp_opt(ts_secs, 0)
            .single()
            .ok_or_else(|| DomainError::InsufficientData(format!("alchemy: bad timestamp {ts_secs}")))
    }
}

/// Build the `topics` array for an ERC-20 Transfer filter. Slot 0 is the
/// event signature hash, slot 1 is the optional `from` (padded), slot 2 is
/// the optional `to` (padded). `null` means "any" for that slot. Slot 3 is
/// omitted so ERC-721 Transfer events (which have an indexed tokenId there)
/// fall in too; the mapper filters them out by checking topics.len() == 3.
fn build_transfer_topics(
    topic_from: Option<&str>,
    topic_to: Option<&str>,
) -> serde_json::Value {
    let null = serde_json::Value::Null;
    let f = topic_from
        .map(|s| json!(s))
        .unwrap_or(null.clone());
    let t = topic_to.map(|s| json!(s)).unwrap_or(null);
    json!([TRANSFER_TOPIC, f, t])
}

/// Map an Alchemy `trace_filter` row into a native-ETH Transfer. Filters:
/// * `error` field present → sub-call reverted, no value moved.
/// * `delegatecall` / `staticcall` → no value transfer by semantics.
/// * `value == 0` → nothing to record.
/// * `traceAddress == []` AND outer `type == "call"` — the outermost trace
///   IS the outer tx's value transfer (idx=0); inner traces shift by +1.
fn map_trace_to_transfer(
    chain: ChainId,
    raw: &serde_json::Value,
    block_ts: &HashMap<u64, chrono::DateTime<chrono::Utc>>,
    by_tx: &mut HashMap<[u8; 32], u32>,
) -> anyhow::Result<Option<Transfer>> {
    use anyhow::{Context, anyhow};

    if raw.get("error").and_then(|v| v.as_str()).is_some() {
        return Ok(None);
    }

    let trace_type = raw
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("trace: missing type"))?;
    let action = raw
        .get("action")
        .ok_or_else(|| anyhow!("trace: missing action"))?;

    let (from_s, to_s, value_s) = match trace_type {
        "call" => {
            let call_type = action.get("callType").and_then(|v| v.as_str()).unwrap_or("call");
            if matches!(call_type, "delegatecall" | "staticcall") {
                return Ok(None);
            }
            let from_s = action
                .get("from")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("trace.call: missing from"))?;
            let to_s = action
                .get("to")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("trace.call: missing to"))?;
            let value_s = action
                .get("value")
                .and_then(|v| v.as_str())
                .unwrap_or("0x0");
            (from_s, to_s, value_s)
        }
        "create" => {
            let from_s = action
                .get("from")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("trace.create: missing from"))?;
            let to_s = raw
                .get("result")
                .and_then(|r| r.get("address"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("trace.create: missing result.address"))?;
            let value_s = action
                .get("value")
                .and_then(|v| v.as_str())
                .unwrap_or("0x0");
            (from_s, to_s, value_s)
        }
        "suicide" => {
            let from_s = action
                .get("address")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("trace.suicide: missing address"))?;
            let to_s = action
                .get("refundAddress")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("trace.suicide: missing refundAddress"))?;
            let value_s = action
                .get("balance")
                .and_then(|v| v.as_str())
                .unwrap_or("0x0");
            (from_s, to_s, value_s)
        }
        other => {
            tracing::debug!(trace_type = other, "alchemy: unknown trace type, skip");
            return Ok(None);
        }
    };

    let raw_val = parse_hex_u256(value_s).context("trace: value")?;
    if raw_val.is_zero() {
        return Ok(None);
    }

    let tx_hash_s = raw
        .get("transactionHash")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("trace: missing transactionHash"))?;
    let block_num = raw
        .get("blockNumber")
        .and_then(|v| v.as_u64())
        .or_else(|| {
            raw.get("blockNumber")
                .and_then(|v| v.as_str())
                .and_then(|s| parse_hex_u64(s).ok())
        })
        .ok_or_else(|| anyhow!("trace: missing blockNumber"))?;
    let block_hash_s = raw.get("blockHash").and_then(|v| v.as_str());

    let tx_hash = parse_hash32(tx_hash_s).context("trace: tx hash")?;
    let block_hash = block_hash_s
        .map(parse_hash32)
        .transpose()
        .context("trace: block hash")?
        .unwrap_or(tx_hash);

    let from = parse_eth_address(chain, from_s).context("trace: from")?;
    let to = parse_eth_address(chain, to_s).context("trace: to")?;

    let timestamp = match block_ts.get(&block_num) {
        Some(t) => *t,
        None => chrono::Utc.timestamp_opt(0, 0).single().unwrap(),
    };

    // Outer trace (traceAddress == []) covers the outer tx value transfer
    // → claim idx=0. Inner traces shift by +1 per tx so they never collide
    // on (chain, tx_hash, idx).
    let trace_addr_empty = raw
        .get("traceAddress")
        .and_then(|v| v.as_array())
        .map(|a| a.is_empty())
        .unwrap_or(true);
    let idx = if trace_addr_empty && trace_type == "call" {
        // First reservation per tx.
        by_tx.entry(tx_hash).or_insert(0);
        0
    } else {
        let position = by_tx.entry(tx_hash).or_insert(0);
        let i = position.saturating_add(1);
        *position += 1;
        i
    };

    Ok(Some(Transfer::new(
        TransferId::new(chain, tx_hash, idx),
        chain,
        TxRef::new(chain, tx_hash),
        from,
        to,
        AssetId::native(chain),
        Amount::new(raw_val, 18),
        BlockRef::new(chain, block_num, block_hash),
        timestamp,
        TransferKind::Native,
        Finality::Confirmed,
    )))
}

/// Map a single `eth_getBlockByNumber(includeTxs=true)` transaction row
/// into a native Transfer. Filters:
/// * Contract-creation tx (`to == null`) — value still moves into the new
///   contract; pull the destination from the receipt only if we had it.
///   In this MVP we drop creation txs (the inner `transfers_for_address`
///   trace_filter pathway picks them up via the create trace).
/// * Value == 0 — skip (pure contract call).
fn map_native_tx_to_transfer(
    chain: ChainId,
    raw: &serde_json::Value,
    block_number: u64,
    block_hash: [u8; 32],
    timestamp: chrono::DateTime<chrono::Utc>,
    by_tx: &mut HashMap<[u8; 32], u32>,
) -> anyhow::Result<Option<Transfer>> {
    use anyhow::{Context, anyhow};

    let to_s = match raw.get("to").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(s) => s,
        // Contract creation — outer tx's value goes into the deployed
        // contract, but `to` is null on the body. Skip; the trace-filter
        // path captures the same transfer via a CREATE row.
        None => return Ok(None),
    };
    let value_s = raw
        .get("value")
        .and_then(|v| v.as_str())
        .unwrap_or("0x0");
    let raw_val = parse_hex_u256(value_s).context("tx: value")?;
    if raw_val.is_zero() {
        return Ok(None);
    }
    let from_s = raw
        .get("from")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("tx: missing from"))?;
    let tx_hash_s = raw
        .get("hash")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("tx: missing hash"))?;
    let tx_hash = parse_hash32(tx_hash_s).context("tx: hash")?;
    let from = parse_eth_address(chain, from_s).context("tx: from")?;
    let to = parse_eth_address(chain, to_s).context("tx: to")?;

    // Reserve idx=0 for the outer native transfer — subsequent log/internal
    // entries for the same tx start at idx=1.
    by_tx.entry(tx_hash).or_insert(0);

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
        Finality::Confirmed,
    )))
}

/// Map an Alchemy `eth_getLogs` row (ERC-20 Transfer) into a Transfer.
/// Filters ERC-721 by requiring exactly 3 topics (event sig + from + to).
fn map_log_to_transfer(
    chain: ChainId,
    raw: &serde_json::Value,
    block_ts: &HashMap<u64, chrono::DateTime<chrono::Utc>>,
    by_tx: &mut HashMap<[u8; 32], u32>,
) -> anyhow::Result<Option<Transfer>> {
    use anyhow::{Context, anyhow};

    let topics = raw
        .get("topics")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("log: missing topics"))?;
    if topics.len() != 3 {
        // ERC-721 (4 topics) or non-Transfer event reaching our filter.
        return Ok(None);
    }
    let from_topic = topics[1].as_str().ok_or_else(|| anyhow!("log: topic1 not str"))?;
    let to_topic = topics[2].as_str().ok_or_else(|| anyhow!("log: topic2 not str"))?;
    let from = parse_eth_address(chain, &unpad_address(from_topic))
        .context("log: from address")?;
    let to = parse_eth_address(chain, &unpad_address(to_topic))
        .context("log: to address")?;

    let data = raw.get("data").and_then(|v| v.as_str()).unwrap_or("0x");
    let raw_val = parse_hex_u256(data).context("log: data")?;
    if raw_val.is_zero() {
        return Ok(None);
    }

    let contract_s = raw
        .get("address")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("log: missing address"))?;
    let contract = parse_eth_address(chain, contract_s).context("log: contract")?;

    let tx_hash_s = raw
        .get("transactionHash")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("log: missing transactionHash"))?;
    let block_num_s = raw
        .get("blockNumber")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("log: missing blockNumber"))?;
    let block_hash_s = raw.get("blockHash").and_then(|v| v.as_str());

    let tx_hash = parse_hash32(tx_hash_s).context("log: tx hash")?;
    let block_hash = block_hash_s
        .map(parse_hash32)
        .transpose()
        .context("log: block hash")?
        .unwrap_or(tx_hash);
    let block_num = parse_hex_u64(block_num_s).context("log: blockNumber")?;

    let timestamp = match block_ts.get(&block_num) {
        Some(t) => *t,
        None => chrono::Utc.timestamp_opt(0, 0).single().unwrap(),
    };

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
        // Decimals are unknown from the Transfer event alone; default to 18
        // and let downstream asset enrichment correct it. Same compromise
        // that the moralis/bigquery sources make on missing-decimal events.
        Amount::new(raw_val, 18),
        BlockRef::new(chain, block_num, block_hash),
        timestamp,
        TransferKind::Token {
            contract,
            standard: TokenStandard::Erc20,
            symbol: None,
        },
        Finality::Confirmed,
    )))
}

/// Heuristic match for Alchemy responses that signal an `eth_getLogs` window
/// exceeded the response-size cap and needs to be subdivided. The exact
/// wording has drifted between Alchemy versions ("query returned more than
/// 10000 results", "log response size exceeded", "response size too large",
/// "response size limit"); accepting any of these — but nothing else —
/// keeps the bisection branch firing on real overflow without swallowing
/// unrelated errors that mention "log".
fn is_log_response_too_large(msg: &str) -> bool {
    let l = msg.to_ascii_lowercase();
    l.contains("log response size exceeded")
        || l.contains("response size exceeded")
        || l.contains("response size too large")
        || l.contains("response size limit")
        || l.contains("query returned more than")
        || l.contains("too many results")
        || l.contains("result window")
}

fn parse_hex_u64(s: &str) -> anyhow::Result<u64> {
    let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    Ok(u64::from_str_radix(s, 16)?)
}

fn parse_hex_u256(s: &str) -> anyhow::Result<U256> {
    let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    if s.is_empty() {
        return Ok(U256::zero());
    }
    Ok(U256::from_str_radix(s, 16)?)
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

/// Strip the 12-byte left-pad from a topic-encoded address.
fn unpad_address(topic: &str) -> String {
    let s = topic.strip_prefix("0x").unwrap_or(topic);
    if s.len() >= 40 {
        format!("0x{}", &s[s.len() - 40..])
    } else {
        format!("0x{s}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpad_address_strips_left_pad() {
        let topic = "0x000000000000000000000000dac17f958d2ee523a2206206994597c13d831ec7";
        assert_eq!(unpad_address(topic), "0xdac17f958d2ee523a2206206994597c13d831ec7");
    }

    #[test]
    fn parse_hex_u64_handles_prefix_and_case() {
        assert_eq!(parse_hex_u64("0x10").unwrap(), 16);
        assert_eq!(parse_hex_u64("0X10").unwrap(), 16);
        assert_eq!(parse_hex_u64("ff").unwrap(), 255);
    }

    #[test]
    fn parse_hex_u256_empty_is_zero() {
        assert!(parse_hex_u256("0x").unwrap().is_zero());
        assert!(parse_hex_u256("").unwrap().is_zero());
    }

    #[test]
    fn build_transfer_topics_full_filter() {
        let v = build_transfer_topics(Some("0xfrom"), Some("0xto"));
        assert_eq!(v[0], TRANSFER_TOPIC);
        assert_eq!(v[1], "0xfrom");
        assert_eq!(v[2], "0xto");
    }

    #[test]
    fn build_transfer_topics_from_only() {
        let v = build_transfer_topics(Some("0xfrom"), None);
        assert_eq!(v[1], "0xfrom");
        assert!(v[2].is_null());
    }

    #[test]
    fn bisection_trigger_matches_known_alchemy_phrasings() {
        // The canonical wordings we've observed from Alchemy across versions.
        assert!(is_log_response_too_large(
            "alchemy eth_getLogs rpc error -32602: query returned more than 10000 results"
        ));
        assert!(is_log_response_too_large(
            "Log response size exceeded. You can make eth_getLogs requests with up to a 2K block range."
        ));
        assert!(is_log_response_too_large(
            "response size exceeded the limit"
        ));
        assert!(is_log_response_too_large("response size too large"));
        assert!(is_log_response_too_large(
            "Response size limit reached, please use a smaller range"
        ));
        assert!(is_log_response_too_large("too many results"));
        // Newer wording observed in Alchemy proxy:
        assert!(is_log_response_too_large("result window too large"));
    }

    #[test]
    fn bisection_trigger_ignores_unrelated_errors() {
        // The previous heuristic was `contains("log")` which fires on
        // *anything* mentioning logging — including innocuous library
        // errors. The hardened matcher must NOT trip on these.
        assert!(!is_log_response_too_large(
            "tracing log subscriber failed to initialize"
        ));
        assert!(!is_log_response_too_large(
            "method eth_getLogs not enabled on this endpoint"
        ));
        assert!(!is_log_response_too_large(
            "invalid argument 0: hex string of odd length"
        ));
        assert!(!is_log_response_too_large("rate limit reached"));
        assert!(!is_log_response_too_large(""));
    }
}
