use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use domain::{
    chain::ChainId,
    error::{DomainError, DomainResult},
    ports::{BlockRange, ChainSource},
    primitives::{Address, BlockRef},
    transfer::{NormalizedBlock, Transfer},
};

const DEFAULT_SOURCE_COOLDOWN_SECS: u64 = 10;

/// Per-source circuit-breaker state: when does the cooldown lapse, and
/// what was the rate-limit reason that triggered it. Reason is preserved
/// across the cooldown window so subsequent failover logs can show *why*
/// a source is currently being skipped, not just "all cooled before attempt".
struct CooldownState {
    until_ms: AtomicU64,
    reason: Mutex<String>,
}

impl CooldownState {
    fn new() -> Self {
        Self {
            until_ms: AtomicU64::new(0),
            reason: Mutex::new(String::new()),
        }
    }

    /// Extend the cooldown to `new_until_ms` if it pushes the deadline
    /// later, and record `new_reason`. Concurrent calls during the same
    /// window can race; whichever lands last wins the reason, but the
    /// deadline only ever moves forward (via fetch_max).
    fn record(&self, new_until_ms: u64, new_reason: String) {
        let prev = self.until_ms.fetch_max(new_until_ms, Ordering::Relaxed);
        if prev <= new_until_ms {
            if let Ok(mut r) = self.reason.lock() {
                *r = new_reason;
            }
        }
    }

    fn snapshot(&self) -> (u64, String) {
        let until = self.until_ms.load(Ordering::Relaxed);
        let reason = self
            .reason
            .lock()
            .map(|r| r.clone())
            .unwrap_or_default();
        (until, reason)
    }
}

/// Names of the capabilities the router can dispatch. Each capability has
/// its own ordered chain of source names; the router tries them in order
/// and fails over from `RateLimited` to the next entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    Transfers,
    IsContract,
    LatestBlock,
    FetchBlock,
}

impl Capability {
    fn as_str(self) -> &'static str {
        match self {
            Capability::Transfers => "transfers",
            Capability::IsContract => "is_contract",
            Capability::LatestBlock => "latest_block",
            Capability::FetchBlock => "fetch_block",
        }
    }
}

/// Per-capability chain of source NAMES (not Arcs) — the actual sources
/// are stored once in `RoutedEvmSourceBuilder::sources` so circuit-breaker
/// state is shared across capabilities for the same underlying upstream.
#[derive(Debug, Clone, Default)]
pub struct RoutedChains {
    pub transfers: Vec<String>,
    pub is_contract: Vec<String>,
    pub latest_block: Vec<String>,
    pub fetch_block: Vec<String>,
}

pub struct RoutedEvmSourceBuilder {
    chain_id: ChainId,
    sources: HashMap<String, Arc<dyn ChainSource>>,
    chains: RoutedChains,
    source_cooldown: Duration,
}

impl RoutedEvmSourceBuilder {
    pub fn new(chain_id: ChainId) -> Self {
        Self {
            chain_id,
            sources: HashMap::new(),
            chains: RoutedChains::default(),
            source_cooldown: Duration::from_secs(DEFAULT_SOURCE_COOLDOWN_SECS),
        }
    }

    pub fn register<S: ChainSource + 'static>(mut self, name: impl Into<String>, source: S) -> Self {
        self.sources.insert(name.into(), Arc::new(source));
        self
    }

    pub fn register_arc(mut self, name: impl Into<String>, source: Arc<dyn ChainSource>) -> Self {
        self.sources.insert(name.into(), source);
        self
    }

    pub fn chains(mut self, chains: RoutedChains) -> Self {
        self.chains = chains;
        self
    }

    pub fn source_cooldown(mut self, cooldown: Duration) -> Self {
        self.source_cooldown = cooldown;
        self
    }

    pub fn build(self) -> anyhow::Result<RoutedEvmSource> {
        // Every name referenced in any chain must resolve to a registered
        // source — surfacing typos at boot beats silent fallbacks later.
        let mut all_referenced: HashSet<&str> = HashSet::new();
        for v in [
            &self.chains.transfers,
            &self.chains.is_contract,
            &self.chains.latest_block,
            &self.chains.fetch_block,
        ] {
            for n in v {
                all_referenced.insert(n.as_str());
            }
        }
        for name in &all_referenced {
            if !self.sources.contains_key(*name) {
                anyhow::bail!(
                    "RoutedEvmSource: chain references unknown source '{name}' — \
                     register it via .register('{name}', …) before .build()"
                );
            }
        }

        // Empty chains are allowed (caller may legitimately not need
        // fetch_block, for instance). Methods on empty chains return a
        // clear error so misconfig manifests visibly.

        let cooldowns: HashMap<String, Arc<CooldownState>> = self
            .sources
            .keys()
            .map(|n| (n.clone(), Arc::new(CooldownState::new())))
            .collect();

        tracing::info!(
            chain_id = ?self.chain_id,
            sources = ?self.sources.keys().collect::<Vec<_>>(),
            transfers = ?self.chains.transfers,
            is_contract = ?self.chains.is_contract,
            latest_block = ?self.chains.latest_block,
            fetch_block = ?self.chains.fetch_block,
            source_cooldown_secs = self.source_cooldown.as_secs(),
            "Routed ETH source initialized"
        );

        Ok(RoutedEvmSource {
            chain_id: self.chain_id,
            sources: self.sources,
            cooldowns,
            chains: self.chains,
            source_cooldown: self.source_cooldown,
        })
    }
}

/// `ChainSource` implementation that fans out per-method calls to a chain
/// of underlying sources. On `RateLimited` from one source we cool it
/// (per-source circuit breaker) and try the next; any other error / success
/// is returned to the caller verbatim.
///
/// Cooldown state is shared per source across all capability chains — if
/// Etherscan rate-limits on an `is_contract` attempt, the next `transfers`
/// call (if Etherscan is in that chain too) will skip Etherscan for the
/// same cooldown window. This avoids us hammering an upstream that's
/// already throttling.
pub struct RoutedEvmSource {
    chain_id: ChainId,
    sources: HashMap<String, Arc<dyn ChainSource>>,
    cooldowns: HashMap<String, Arc<CooldownState>>,
    chains: RoutedChains,
    source_cooldown: Duration,
}

/// Snapshot of a cooling source returned alongside live ones so callers
/// can attribute a "no sources available" failure to a specific reason.
#[derive(Debug, Clone)]
struct CooledEntry {
    name: String,
    remaining_ms: u64,
    reason: String,
}

impl std::fmt::Display for CooledEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = if self.reason.is_empty() {
            "<no recorded reason>"
        } else {
            self.reason.as_str()
        };
        write!(
            f,
            "{} (cooling {}ms more, was: {reason})",
            self.name, self.remaining_ms
        )
    }
}

impl RoutedEvmSource {
    pub fn builder(chain_id: ChainId) -> RoutedEvmSourceBuilder {
        RoutedEvmSourceBuilder::new(chain_id)
    }

    fn chain_for(&self, cap: Capability) -> &[String] {
        match cap {
            Capability::Transfers => &self.chains.transfers,
            Capability::IsContract => &self.chains.is_contract,
            Capability::LatestBlock => &self.chains.latest_block,
            Capability::FetchBlock => &self.chains.fetch_block,
        }
    }

    /// Partition `cap`'s chain into still-live entries (ready to call) and
    /// cooled snapshots (with remaining cooldown + last reason). The
    /// cooled list feeds the final error message when every source in the
    /// chain is unavailable, so the operator can see *why* nothing was
    /// tried instead of just "all cooled before attempt".
    fn live_entries(
        &self,
        cap: Capability,
    ) -> (Vec<(String, Arc<dyn ChainSource>)>, Vec<CooledEntry>) {
        let now = now_unix_ms();
        let mut live: Vec<(String, Arc<dyn ChainSource>)> = Vec::new();
        let mut cooled: Vec<CooledEntry> = Vec::new();
        for name in self.chain_for(cap) {
            let (until, reason) = self
                .cooldowns
                .get(name)
                .map(|s| s.snapshot())
                .unwrap_or((0, String::new()));
            if until > now {
                cooled.push(CooledEntry {
                    name: name.clone(),
                    remaining_ms: until - now,
                    reason,
                });
                continue;
            }
            if let Some(src) = self.sources.get(name).cloned() {
                live.push((name.clone(), src));
            }
        }
        (live, cooled)
    }

    fn cool(&self, name: &str, reason: String) {
        if let Some(c) = self.cooldowns.get(name) {
            let until = now_unix_ms() + self.source_cooldown.as_millis() as u64;
            c.record(until, reason);
        }
    }

    fn empty_chain_error(&self, cap: Capability) -> DomainError {
        DomainError::InsufficientData(format!(
            "router: capability '{}' has no sources configured for chain {:?}",
            cap.as_str(),
            self.chain_id
        ))
    }

    /// Render the per-source breakdown that explains why no live source
    /// could serve the call. Includes both:
    /// * sources tried this call (with the RateLimited message they returned),
    /// * sources skipped because they were already cooling (with remaining
    ///   cooldown + the reason that started it).
    fn format_unavailable(
        tried: &[(String, String)],
        cooled: &[CooledEntry],
    ) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(tried.len() + cooled.len());
        for (name, msg) in tried {
            parts.push(format!("{name} (just rate-limited: {msg})"));
        }
        for c in cooled {
            parts.push(c.to_string());
        }
        if parts.is_empty() {
            "<chain is empty>".to_string()
        } else {
            parts.join("; ")
        }
    }
}

#[async_trait]
impl ChainSource for RoutedEvmSource {
    fn chain_id(&self) -> ChainId {
        self.chain_id
    }

    async fn latest_block(&self) -> DomainResult<BlockRef> {
        let cap = Capability::LatestBlock;
        let (entries, cooled) = self.live_entries(cap);
        if entries.is_empty() && self.chain_for(cap).is_empty() {
            return Err(self.empty_chain_error(cap));
        }
        let mut tried: Vec<(String, String)> = Vec::new();
        for (name, src) in entries {
            match src.latest_block().await {
                Ok(v) => return Ok(v),
                Err(DomainError::RateLimited(msg)) => {
                    tracing::warn!(
                        source = %name,
                        capability = cap.as_str(),
                        reason = %msg,
                        "router: source rate-limited, failing over"
                    );
                    self.cool(&name, msg.clone());
                    tried.push((name, msg));
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        let breakdown = Self::format_unavailable(&tried, &cooled);
        tracing::warn!(
            capability = cap.as_str(),
            breakdown = %breakdown,
            "router: all sources rate-limited"
        );
        Err(DomainError::RateLimited(format!(
            "router: all sources for {} rate-limited [{breakdown}]",
            cap.as_str()
        )))
    }

    async fn fetch_block(&self, height: u64) -> DomainResult<NormalizedBlock> {
        let cap = Capability::FetchBlock;
        let (entries, cooled) = self.live_entries(cap);
        if entries.is_empty() && self.chain_for(cap).is_empty() {
            return Err(self.empty_chain_error(cap));
        }
        let mut tried: Vec<(String, String)> = Vec::new();
        for (name, src) in entries {
            match src.fetch_block(height).await {
                Ok(v) => return Ok(v),
                Err(DomainError::RateLimited(msg)) => {
                    tracing::warn!(
                        source = %name,
                        capability = cap.as_str(),
                        reason = %msg,
                        "router: source rate-limited, failing over"
                    );
                    self.cool(&name, msg.clone());
                    tried.push((name, msg));
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        let breakdown = Self::format_unavailable(&tried, &cooled);
        tracing::warn!(
            capability = cap.as_str(),
            breakdown = %breakdown,
            "router: all sources rate-limited"
        );
        Err(DomainError::RateLimited(format!(
            "router: all sources for {} rate-limited [{breakdown}]",
            cap.as_str()
        )))
    }

    async fn transfers_for_address(
        &self,
        addr: &Address,
        range: BlockRange,
        max_transfers: usize,
    ) -> DomainResult<Vec<Transfer>> {
        let cap = Capability::Transfers;
        let (entries, cooled) = self.live_entries(cap);
        if entries.is_empty() && self.chain_for(cap).is_empty() {
            return Err(self.empty_chain_error(cap));
        }
        let mut tried: Vec<(String, String)> = Vec::new();
        for (name, src) in entries {
            match src.transfers_for_address(addr, range, max_transfers).await {
                Ok(v) => return Ok(v),
                Err(DomainError::RateLimited(msg)) => {
                    tracing::warn!(
                        source = %name,
                        capability = cap.as_str(),
                        reason = %msg,
                        "router: source rate-limited, failing over"
                    );
                    self.cool(&name, msg.clone());
                    tried.push((name, msg));
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        let breakdown = Self::format_unavailable(&tried, &cooled);
        tracing::warn!(
            capability = cap.as_str(),
            breakdown = %breakdown,
            "router: all sources rate-limited"
        );
        Err(DomainError::RateLimited(format!(
            "router: all sources for {} rate-limited [{breakdown}]",
            cap.as_str()
        )))
    }

    async fn is_contract(&self, addr: &Address) -> DomainResult<Option<bool>> {
        let cap = Capability::IsContract;
        let (entries, cooled) = self.live_entries(cap);
        if entries.is_empty() && self.chain_for(cap).is_empty() {
            // Unlike other methods, is_contract is allowed to gracefully
            // return Unknown when nothing's configured — downstream pattern
            // detectors degrade rather than blow up.
            return Ok(None);
        }
        let mut tried: Vec<(String, String)> = Vec::new();
        for (name, src) in entries {
            match src.is_contract(addr).await {
                Ok(v) => return Ok(v),
                Err(DomainError::RateLimited(msg)) => {
                    tracing::warn!(
                        source = %name,
                        capability = cap.as_str(),
                        reason = %msg,
                        "router: source rate-limited, failing over"
                    );
                    self.cool(&name, msg.clone());
                    tried.push((name, msg));
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        // All sources rate-limited → return Unknown rather than error.
        // is_contract is best-effort context; pattern detectors handle None.
        let breakdown = Self::format_unavailable(&tried, &cooled);
        tracing::warn!(
            capability = cap.as_str(),
            breakdown = %breakdown,
            "router: all is_contract sources rate-limited, returning Unknown"
        );
        Ok(None)
    }

    /// Same routing rules as the single-shot `is_contract` (per-source
    /// failover on RateLimited), but each candidate source gets the
    /// *entire* batch in one call. So the per-source cache (Etherscan's
    /// harvest cache, Alchemy's own moka cache) gets to filter out hits
    /// before the second source ever sees the residue.
    async fn is_contract_batch(
        &self,
        addrs: &[Address],
    ) -> DomainResult<Vec<Option<bool>>> {
        let cap = Capability::IsContract;
        let (entries, cooled) = self.live_entries(cap);
        if entries.is_empty() && self.chain_for(cap).is_empty() {
            return Ok(vec![None; addrs.len()]);
        }
        let mut tried: Vec<(String, String)> = Vec::new();
        // Track which slots are still None so we can hand only the
        // unresolved residue to the next source — saves work on chained
        // [etherscan, alchemy] config where harvest covers most addresses.
        let mut current_addrs: Vec<Address> = addrs.to_vec();
        let mut slot_map: Vec<usize> = (0..addrs.len()).collect();
        let mut final_out: Vec<Option<bool>> = vec![None; addrs.len()];

        for (name, src) in entries {
            if current_addrs.is_empty() {
                break;
            }
            match src.is_contract_batch(&current_addrs).await {
                Ok(partial) => {
                    let mut next_addrs: Vec<Address> = Vec::new();
                    let mut next_slots: Vec<usize> = Vec::new();
                    for (local_i, v) in partial.into_iter().enumerate() {
                        let original_slot = slot_map[local_i];
                        match v {
                            Some(_) => final_out[original_slot] = v,
                            None => {
                                next_addrs.push(current_addrs[local_i].clone());
                                next_slots.push(original_slot);
                            }
                        }
                    }
                    current_addrs = next_addrs;
                    slot_map = next_slots;
                    // Resolved by THIS source; if every address landed,
                    // we're done early. Otherwise the next source in the
                    // chain gets a turn at the leftovers.
                }
                Err(DomainError::RateLimited(msg)) => {
                    tracing::warn!(
                        source = %name,
                        capability = cap.as_str(),
                        reason = %msg,
                        batch = current_addrs.len(),
                        "router: source rate-limited on batch, failing over"
                    );
                    self.cool(&name, msg.clone());
                    tried.push((name, msg));
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        if !current_addrs.is_empty() {
            // Two qualitatively different reasons for unresolved tail:
            // * sources all rate-limited / cooled → format_unavailable
            //   shows per-source cause;
            // * sources returned Ok but gave us None for those addresses
            //   (genuine "we don't know" — typically because the upstream
            //   doesn't have data, OR a parsing/shape bug). The previous
            //   log called this "chain is empty" which read as a config
            //   bug; distinguish so operators don't chase phantom configs.
            if tried.is_empty() && cooled.is_empty() {
                tracing::warn!(
                    capability = cap.as_str(),
                    unresolved = current_addrs.len(),
                    total = addrs.len(),
                    "router: is_contract_batch finished with unresolved addresses — sources returned Ok with None (no rate-limit). Upstream may be returning errors or an unexpected shape; check the per-source logs."
                );
            } else {
                let breakdown = Self::format_unavailable(&tried, &cooled);
                tracing::warn!(
                    capability = cap.as_str(),
                    unresolved = current_addrs.len(),
                    total = addrs.len(),
                    breakdown = %breakdown,
                    "router: is_contract_batch finished with unresolved addresses — every source was rate-limited"
                );
            }
        }
        Ok(final_out)
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::primitives::Address;
    use std::sync::Mutex;

    /// In-memory fake source whose behaviour per-method is scripted by the
    /// test (Ok / RateLimited / other Err), with a call counter we can
    /// assert on to prove failover routing.
    struct FakeSource {
        chain_id: ChainId,
        latest: Mutex<Vec<DomainResult<BlockRef>>>,
        is_contract: Mutex<Vec<DomainResult<Option<bool>>>>,
        latest_calls: Arc<AtomicU64>,
        is_contract_calls: Arc<AtomicU64>,
    }

    impl FakeSource {
        fn new(chain_id: ChainId) -> Self {
            Self {
                chain_id,
                latest: Mutex::new(Vec::new()),
                is_contract: Mutex::new(Vec::new()),
                latest_calls: Arc::new(AtomicU64::new(0)),
                is_contract_calls: Arc::new(AtomicU64::new(0)),
            }
        }

        fn queue_latest(self, r: DomainResult<BlockRef>) -> Self {
            self.latest.lock().unwrap().push(r);
            self
        }

        fn queue_is_contract(self, r: DomainResult<Option<bool>>) -> Self {
            self.is_contract.lock().unwrap().push(r);
            self
        }
    }

    #[async_trait]
    impl ChainSource for FakeSource {
        fn chain_id(&self) -> ChainId {
            self.chain_id
        }
        async fn latest_block(&self) -> DomainResult<BlockRef> {
            self.latest_calls.fetch_add(1, Ordering::Relaxed);
            self.latest
                .lock()
                .unwrap()
                .remove(0)
        }
        async fn fetch_block(&self, _h: u64) -> DomainResult<NormalizedBlock> {
            unimplemented!()
        }
        async fn transfers_for_address(
            &self,
            _a: &Address,
            _r: BlockRange,
            _m: usize,
        ) -> DomainResult<Vec<Transfer>> {
            unimplemented!()
        }
        async fn is_contract(&self, _addr: &Address) -> DomainResult<Option<bool>> {
            self.is_contract_calls.fetch_add(1, Ordering::Relaxed);
            self.is_contract
                .lock()
                .unwrap()
                .remove(0)
        }
    }

    #[tokio::test]
    async fn failover_on_rate_limit_calls_second_source() {
        let a = FakeSource::new(ChainId::ETH)
            .queue_latest(Err(DomainError::RateLimited("a".into())));
        let a_calls = Arc::clone(&a.latest_calls);
        let b = FakeSource::new(ChainId::ETH)
            .queue_latest(Ok(BlockRef::new(ChainId::ETH, 42, [0u8; 32])));
        let b_calls = Arc::clone(&b.latest_calls);

        let router = RoutedEvmSource::builder(ChainId::ETH)
            .register("a", a)
            .register("b", b)
            .chains(RoutedChains {
                latest_block: vec!["a".into(), "b".into()],
                ..Default::default()
            })
            .build()
            .unwrap();

        let r = router.latest_block().await.unwrap();
        assert_eq!(r.height(), 42);
        assert_eq!(a_calls.load(Ordering::Relaxed), 1);
        assert_eq!(b_calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn other_errors_do_not_failover() {
        let a = FakeSource::new(ChainId::ETH)
            .queue_latest(Err(DomainError::InsufficientData("nope".into())));
        let a_calls = Arc::clone(&a.latest_calls);
        let b = FakeSource::new(ChainId::ETH)
            .queue_latest(Ok(BlockRef::new(ChainId::ETH, 1, [0u8; 32])));
        let b_calls = Arc::clone(&b.latest_calls);

        let router = RoutedEvmSource::builder(ChainId::ETH)
            .register("a", a)
            .register("b", b)
            .chains(RoutedChains {
                latest_block: vec!["a".into(), "b".into()],
                ..Default::default()
            })
            .build()
            .unwrap();

        let r = router.latest_block().await;
        assert!(matches!(r, Err(DomainError::InsufficientData(_))));
        assert_eq!(a_calls.load(Ordering::Relaxed), 1);
        assert_eq!(b_calls.load(Ordering::Relaxed), 0, "no failover on non-rate-limit");
    }

    #[tokio::test]
    async fn cooled_source_skipped_within_window() {
        let a = FakeSource::new(ChainId::ETH)
            .queue_latest(Err(DomainError::RateLimited("a".into())))
            .queue_latest(Ok(BlockRef::new(ChainId::ETH, 9, [0u8; 32])));
        let a_calls = Arc::clone(&a.latest_calls);
        let b = FakeSource::new(ChainId::ETH)
            .queue_latest(Ok(BlockRef::new(ChainId::ETH, 1, [0u8; 32])))
            .queue_latest(Ok(BlockRef::new(ChainId::ETH, 2, [0u8; 32])));
        let b_calls = Arc::clone(&b.latest_calls);

        let router = RoutedEvmSource::builder(ChainId::ETH)
            .register("a", a)
            .register("b", b)
            .chains(RoutedChains {
                latest_block: vec!["a".into(), "b".into()],
                ..Default::default()
            })
            .source_cooldown(Duration::from_secs(60))
            .build()
            .unwrap();

        // First call: a fails over to b.
        let r1 = router.latest_block().await.unwrap();
        assert_eq!(r1.height(), 1);
        // Second call: a is cooled → skipped, b serves again. `a` must NOT
        // receive the second call.
        let r2 = router.latest_block().await.unwrap();
        assert_eq!(r2.height(), 2);
        assert_eq!(a_calls.load(Ordering::Relaxed), 1, "cooled source must not be retried");
        assert_eq!(b_calls.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn is_contract_unknown_when_all_rate_limited() {
        // Rate-limit shouldn't bubble for is_contract — pattern detectors
        // treat None as soft-unknown which is the right degradation mode.
        let a = FakeSource::new(ChainId::ETH)
            .queue_is_contract(Err(DomainError::RateLimited("a".into())));
        let router = RoutedEvmSource::builder(ChainId::ETH)
            .register("a", a)
            .chains(RoutedChains {
                is_contract: vec!["a".into()],
                ..Default::default()
            })
            .build()
            .unwrap();

        let addr = Address::new(ChainId::ETH, vec![0u8; 20]);
        let r = router.is_contract(&addr).await.unwrap();
        assert_eq!(r, None);
    }

    #[test]
    fn build_rejects_unknown_source_name() {
        let res = RoutedEvmSource::builder(ChainId::ETH)
            .chains(RoutedChains {
                transfers: vec!["ghost".into()],
                ..Default::default()
            })
            .build();
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn error_message_carries_actual_rate_limit_reasons() {
        // Two sources, both rate-limited in the SAME call (live, both tried).
        // The final RateLimited error must surface BOTH source names and
        // BOTH original reasons — not a generic "all cooled" string.
        let a = FakeSource::new(ChainId::ETH)
            .queue_latest(Err(DomainError::RateLimited(
                "etherscan: HTTP 429 (Max calls per sec)".into(),
            )));
        let b = FakeSource::new(ChainId::ETH)
            .queue_latest(Err(DomainError::RateLimited(
                "alchemy: rpc error -32007 Too many requests".into(),
            )));

        let router = RoutedEvmSource::builder(ChainId::ETH)
            .register("etherscan", a)
            .register("alchemy", b)
            .chains(RoutedChains {
                latest_block: vec!["etherscan".into(), "alchemy".into()],
                ..Default::default()
            })
            .build()
            .unwrap();

        let err = router.latest_block().await.unwrap_err();
        let msg = err.to_string();
        // Both source names present
        assert!(msg.contains("etherscan"), "missing etherscan in: {msg}");
        assert!(msg.contains("alchemy"), "missing alchemy in: {msg}");
        // Both ORIGINAL reasons preserved
        assert!(
            msg.contains("Max calls per sec"),
            "lost etherscan reason: {msg}"
        );
        assert!(
            msg.contains("Too many requests"),
            "lost alchemy reason: {msg}"
        );
    }

    /// Fake source that resolves a fixed map of addresses → bool and
    /// returns None for the rest. Tracks how many times `is_contract_batch`
    /// is invoked so we can assert the router actually batches.
    struct MapSource {
        chain_id: ChainId,
        known: std::collections::HashMap<Vec<u8>, bool>,
        batch_calls: Arc<AtomicU64>,
    }

    impl MapSource {
        fn new(known: Vec<(Vec<u8>, bool)>) -> Self {
            Self {
                chain_id: ChainId::ETH,
                known: known.into_iter().collect(),
                batch_calls: Arc::new(AtomicU64::new(0)),
            }
        }
    }

    #[async_trait]
    impl ChainSource for MapSource {
        fn chain_id(&self) -> ChainId {
            self.chain_id
        }
        async fn latest_block(&self) -> DomainResult<BlockRef> {
            unimplemented!()
        }
        async fn fetch_block(&self, _h: u64) -> DomainResult<NormalizedBlock> {
            unimplemented!()
        }
        async fn transfers_for_address(
            &self,
            _a: &Address,
            _r: BlockRange,
            _m: usize,
        ) -> DomainResult<Vec<Transfer>> {
            unimplemented!()
        }
        async fn is_contract(&self, addr: &Address) -> DomainResult<Option<bool>> {
            Ok(self.known.get(addr.bytes()).copied())
        }
        async fn is_contract_batch(
            &self,
            addrs: &[Address],
        ) -> DomainResult<Vec<Option<bool>>> {
            self.batch_calls.fetch_add(1, Ordering::Relaxed);
            Ok(addrs
                .iter()
                .map(|a| self.known.get(a.bytes()).copied())
                .collect())
        }
    }

    #[tokio::test]
    async fn batch_residue_handed_to_next_source_in_chain() {
        // a knows [A, B]; b knows [C]. Together they cover all three.
        // Router must hand the unresolved residue to b, not the full batch
        // again — saving network calls when chains compose.
        let a = MapSource::new(vec![
            (vec![0xAA; 20], true),
            (vec![0xBB; 20], false),
        ]);
        let a_calls = Arc::clone(&a.batch_calls);
        let b = MapSource::new(vec![(vec![0xCC; 20], true)]);
        let b_calls = Arc::clone(&b.batch_calls);

        let router = RoutedEvmSource::builder(ChainId::ETH)
            .register("a", a)
            .register("b", b)
            .chains(RoutedChains {
                is_contract: vec!["a".into(), "b".into()],
                ..Default::default()
            })
            .build()
            .unwrap();

        let inputs = vec![
            Address::new(ChainId::ETH, vec![0xAA; 20]),
            Address::new(ChainId::ETH, vec![0xBB; 20]),
            Address::new(ChainId::ETH, vec![0xCC; 20]),
            Address::new(ChainId::ETH, vec![0xDD; 20]),
        ];
        let out = router.is_contract_batch(&inputs).await.unwrap();
        assert_eq!(out, vec![Some(true), Some(false), Some(true), None]);
        assert_eq!(a_calls.load(Ordering::Relaxed), 1);
        assert_eq!(b_calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn error_message_explains_cooled_sources_not_tried_this_call() {
        // First call rate-limits source `a`, so the router cools it for
        // 60s. Second call finds NO live source — the error should include
        // the cooldown remaining AND the original reason `a` was cooled,
        // instead of a vacuous "all cooled before attempt".
        let a = FakeSource::new(ChainId::ETH)
            .queue_latest(Err(DomainError::RateLimited(
                "etherscan: daily quota exhausted".into(),
            )));

        let router = RoutedEvmSource::builder(ChainId::ETH)
            .register("etherscan", a)
            .chains(RoutedChains {
                latest_block: vec!["etherscan".into()],
                ..Default::default()
            })
            .source_cooldown(Duration::from_secs(60))
            .build()
            .unwrap();

        // First call: tries etherscan, gets rate-limited, cools it.
        let first = router.latest_block().await.unwrap_err();
        let first_msg = first.to_string();
        assert!(first_msg.contains("daily quota exhausted"));

        // Second call: nothing live, but cooled snapshot should explain.
        let second = router.latest_block().await.unwrap_err();
        let second_msg = second.to_string();
        assert!(
            second_msg.contains("etherscan"),
            "missing source name: {second_msg}"
        );
        assert!(
            second_msg.contains("cooling"),
            "missing cooldown hint: {second_msg}"
        );
        assert!(
            second_msg.contains("daily quota exhausted"),
            "lost original reason on second call: {second_msg}"
        );
        // And specifically the old vacuous wording must NOT be present.
        assert!(
            !second_msg.contains("all cooled before attempt"),
            "regressed to vacuous message: {second_msg}"
        );
    }
}
