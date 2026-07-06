use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use domain::entity::{
    AddressKind, ClusterEvidence, ClusteringHeuristic, EntityCategory, RiskScore, SanctionList,
};
use domain::error::DomainResult;
use domain::ports::{AddressKindRepository, EntityRepository, RiskPort, TransferRepository};
use domain::primitives::{Address, Amount, Confidence, Ratio};
use domain::risk::{RiskEvidence, RiskReport, RiskSignal, RiskSignalKind, SanctionsCheckResult};
use domain::trace::{
    FlowPath, Sink, SinkKind, TaintStrategy, TraceDirection, TraceLimits, TraceOrigin,
    TraceRequest, TraceResult, TraceStats,
};
use domain::transfer::Transfer;
use moka::future::Cache;

use crate::union_find::UnionFind;

#[derive(Debug, Clone)]
pub struct RiskCacheConfig {
    pub score_ttl: Duration,
    pub score_max_entries: u64,
    pub sanctions_ttl: Duration,
    pub sanctions_max_entries: u64,
    pub trace_ttl: Duration,
    pub trace_max_entries: u64,
}

impl Default for RiskCacheConfig {
    fn default() -> Self {
        Self {
            score_ttl: Duration::from_secs(300),
            score_max_entries: 10_000,
            sanctions_ttl: Duration::from_secs(900),
            sanctions_max_entries: 10_000,
            trace_ttl: Duration::from_secs(300),
            trace_max_entries: 2_000,
        }
    }
}

/// Stable key for caching TraceResult.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct TraceCacheKey(String);

impl TraceCacheKey {
    fn build(req: &domain::trace::TraceRequest) -> Self {
        use domain::trace::TraceOrigin;
        let origin = match req.origin() {
            TraceOrigin::Address(a) => format!("addr:{}:{}", a.chain().value(), hex::encode(a.bytes())),
            TraceOrigin::Transaction { chain, hash } => {
                format!("tx:{}:{}", chain.value(), hex::encode(hash))
            }
            TraceOrigin::Transfer(id) => {
                format!("xfer:{}:{}:{}", id.chain().value(), hex::encode(id.tx_hash()), id.index())
            }
        };
        let key = format!(
            "{origin}|d={:?}|s={:?}|h={}|a={}|p={}|m={:?}|u={}",
            req.direction(),
            req.strategy(),
            req.limits().max_hops(),
            req.limits().max_addresses(),
            req.limits().max_paths(),
            req.limits().min_amount_ratio().map(|r| (r.as_f64() * 10_000.0) as i64),
            req.include_unconfirmed(),
        );
        Self(key)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct HeuristicsConfig {
    pub min_fanout: usize,
    pub min_fanin: usize,
    /// upper bound on unique counterparties for fan-out/fan-in/smurfing/
    /// deposit-reuse to still be treated as a personal-wallet pattern.
    /// Above this, the address behaves like shared infrastructure (an
    /// exchange hot wallet, a router, a popular contract) rather than one
    /// owner, so the detector must not fire — merging through it would
    /// transitively cluster large numbers of unrelated addresses together.
    pub max_fanout: usize,
    pub max_fanin: usize,
    pub fan_window: Duration,
    pub smurf_window: Duration,
    pub smurf_max_depth: u32,
    /// fraction tolerance for fan-in/out amount-similarity filter (0.0 = off).
    pub amount_tolerance: f64,
    /// burst detector — minimum txs/window required to even consider firing.
    pub burst_min_count: usize,
    /// burst detector — window length in seconds.
    pub burst_window: Duration,
    /// burst detector — multiplier over the baseline (median over the trailing
    /// 14 windows) above which the latest window is considered anomalous.
    pub burst_multiplier: f64,
    /// fixed-amount clustering — minimum repetitions of the same USD bucket
    /// (rounded to `fixed_amount_bucket_usd`) to fire.
    pub fixed_amount_min_count: usize,
    /// fixed-amount clustering — USD bucket size used to round amounts.
    pub fixed_amount_bucket_usd: f64,
    /// dwell-time — maximum median dwell (seconds in/out delay) before
    /// pass-through is suspected.
    pub dwell_max_secs: u64,
    /// dwell-time — minimum number of matched in/out pairs to fire.
    pub dwell_min_pairs: usize,
    /// deposit-reuse — minimum incoming transfers to `deposit_addr` before
    /// even considering it (below this there's nothing to "reuse").
    pub deposit_reuse_min_incoming: usize,
    /// deposit-reuse — minimum distinct senders required to fire.
    pub deposit_reuse_min_senders: usize,
}

impl Default for HeuristicsConfig {
    fn default() -> Self {
        Self {
            min_fanout: 5,
            min_fanin: 5,
            max_fanout: 200,
            max_fanin: 200,
            fan_window: Duration::from_secs(86_400),
            smurf_window: Duration::from_secs(86_400),
            smurf_max_depth: 2,
            amount_tolerance: 0.0,
            burst_min_count: 20,
            burst_window: Duration::from_secs(3_600),
            burst_multiplier: 5.0,
            fixed_amount_min_count: 5,
            fixed_amount_bucket_usd: 100.0,
            dwell_max_secs: 600,
            dwell_min_pairs: 5,
            deposit_reuse_min_incoming: 3,
            deposit_reuse_min_senders: 2,
        }
    }
}

/// Strategy for collapsing N RiskSignals into a single overall score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreAggregation {
    /// Worst signal sets the score — legacy behaviour. Drop in if you
    /// want one CRITICAL sink to outweigh five HIGH ones, period.
    Max,
    /// `max_severity + Σ(extra_severity × count_bonus_weight)`, capped at
    /// `max_score_cap`. Multiple independent signals stack so a wallet
    /// with 5 HIGH counterparties scores meaningfully above one with 1.
    /// Signals are deduped by (kind, evidence-target) before aggregation
    /// so the same sink appearing in forward+backward traces counts once.
    WeightedCount,
}

/// Tunables for the per-address risk score. Separate from `HeuristicsConfig`
/// (those drive cluster detection); this drives signal aggregation and the
/// internal trace that `score()` walks to find sink exposure.
#[derive(Debug, Clone, Copy)]
pub struct ScoreConfig {
    pub aggregation: ScoreAggregation,
    /// Fraction of each extra signal's severity that adds to the base
    /// (max) score under `WeightedCount`. 0.1 means each additional
    /// HIGH signal contributes ~7.5 points; 0.5 means ~37.5.
    pub count_bonus_weight: f64,
    /// Signals whose severity is below this threshold don't fire as
    /// sink-exposure evidence. Default 75 = `RiskScore::HIGH`.
    pub sink_severity_threshold: u8,
    /// Final score is clamped to this ceiling. 100 = CRITICAL.
    pub max_score_cap: u8,
    /// Depth, breadth, and per-address transfer limits for the internal
    /// trace `score()` runs to discover sink exposure. Smaller → faster
    /// scoring, less coverage; larger → slower, deeper coverage.
    pub trace_max_depth: u32,
    pub trace_max_nodes: usize,
    pub trace_max_paths: usize,
    /// Minimum fraction of inflow that must reach a sink for the sink to
    /// count as exposure. Percent (0–100). Default 5 = 5%.
    pub trace_min_amount_ratio_percent: u8,
}

impl Default for ScoreConfig {
    fn default() -> Self {
        Self {
            aggregation: ScoreAggregation::WeightedCount,
            count_bonus_weight: 0.1,
            sink_severity_threshold: 75,
            max_score_cap: 100,
            trace_max_depth: 5,
            trace_max_nodes: 200,
            trace_max_paths: 100,
            trace_min_amount_ratio_percent: 5,
        }
    }
}

impl ScoreConfig {
    /// Count of distinct signals after dedup — useful for logs/telemetry
    /// so we can see "scored CRITICAL from 7 raw signals collapsing to 3
    /// unique entries" rather than just the final score.
    pub fn unique_count(&self, signals: &[RiskSignal]) -> usize {
        let mut keys: HashSet<SignalKey> = HashSet::new();
        for s in signals {
            keys.insert(signal_key(s));
        }
        keys.len()
    }

    /// Deduplicate signals by a stable key (kind + the most-distinctive
    /// field of their evidence) and aggregate per the configured strategy.
    /// Returns the overall score.
    pub fn aggregate(&self, signals: &[RiskSignal]) -> RiskScore {
        if signals.is_empty() {
            return RiskScore::CLEAN;
        }
        // Collapse duplicates: same (kind, target) → keep the max severity
        // of the group. The same sink in forward+backward traces is one
        // signal, not two.
        let mut by_key: HashMap<SignalKey, u8> = HashMap::new();
        for s in signals {
            let key = signal_key(s);
            let cur = by_key.get(&key).copied().unwrap_or(0);
            by_key.insert(key, cur.max(s.severity().value()));
        }

        let mut severities: Vec<u8> = by_key.into_values().collect();
        severities.sort_unstable_by(|a, b| b.cmp(a));

        match self.aggregation {
            ScoreAggregation::Max => {
                RiskScore::new(severities[0].min(self.max_score_cap))
            }
            ScoreAggregation::WeightedCount => {
                let max = severities[0] as f64;
                let bonus: f64 = severities[1..]
                    .iter()
                    .map(|&s| s as f64 * self.count_bonus_weight)
                    .sum();
                let total = (max + bonus).round();
                let clamped = total.clamp(0.0, self.max_score_cap as f64) as u8;
                RiskScore::new(clamped)
            }
        }
    }
}

/// Stable identity for dedup. We choose the most-distinctive field of
/// each evidence variant: a sink address, a category discriminant, the
/// pattern string, etc. Same key → same logical signal regardless of
/// which trace direction surfaced it.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
enum SignalKey {
    Sink(Address),
    Category(String),
    Pattern(String),
    Manual(String),
    /// Fallback when evidence carries no distinctive identifier; uses the
    /// signal kind's debug repr so distinct kinds still don't collapse.
    Kind(String),
}

fn signal_key(s: &RiskSignal) -> SignalKey {
    use domain::risk::RiskEvidence;
    match s.evidence() {
        RiskEvidence::SinkExposure(sinks) => {
            // Multiple sinks in one signal — collapse to the address of
            // the first one. In practice we emit one Sink per signal.
            if let Some(first) = sinks.first() {
                return SignalKey::Sink(first.address().clone());
            }
            SignalKey::Kind(format!("{:?}", s.kind()))
        }
        RiskEvidence::EntityCategory(cat) => SignalKey::Category(format!("{:?}", cat)),
        RiskEvidence::TransactionPattern(p) => SignalKey::Pattern(p.clone()),
        RiskEvidence::Manual(m) => SignalKey::Manual(m.clone()),
    }
}

pub struct RiskService<R, E> {
    transfers: R,
    entities: E,
    address_kinds: Option<Arc<dyn AddressKindRepository>>,
    score_cache: Cache<Address, RiskReport>,
    sanctions_cache: Cache<Address, SanctionsCheckResult>,
    trace_cache: Cache<TraceCacheKey, TraceResult>,
    heuristics: HeuristicsConfig,
    score_cfg: ScoreConfig,
}

impl<R, E> RiskService<R, E> {
    pub fn new(
        transfers: R,
        entities: E,
        cache: RiskCacheConfig,
        heuristics: HeuristicsConfig,
    ) -> Self {
        Self::with_score_config(transfers, entities, cache, heuristics, ScoreConfig::default())
    }

    pub fn with_score_config(
        transfers: R,
        entities: E,
        cache: RiskCacheConfig,
        heuristics: HeuristicsConfig,
        score_cfg: ScoreConfig,
    ) -> Self {
        let score_cache = Cache::builder()
            .max_capacity(cache.score_max_entries)
            .time_to_live(cache.score_ttl)
            .build();
        let sanctions_cache = Cache::builder()
            .max_capacity(cache.sanctions_max_entries)
            .time_to_live(cache.sanctions_ttl)
            .build();
        let trace_cache = Cache::builder()
            .max_capacity(cache.trace_max_entries)
            .time_to_live(cache.trace_ttl)
            .build();
        Self {
            transfers,
            entities,
            address_kinds: None,
            score_cache,
            sanctions_cache,
            trace_cache,
            heuristics,
            score_cfg,
        }
    }

    pub fn with_address_kinds(mut self, kinds: Arc<dyn AddressKindRepository>) -> Self {
        self.address_kinds = Some(kinds);
        self
    }
}

impl<R, E> RiskService<R, E>
where
    R: TransferRepository,
    E: EntityRepository,
{
    pub async fn score_batch(&self, addresses: &[Address]) -> DomainResult<Vec<RiskReport>> {
        use futures::stream::{FuturesUnordered, StreamExt};
        let mut futs: FuturesUnordered<_> = addresses.iter().map(|addr| self.score(addr)).collect();
        let mut results = Vec::with_capacity(addresses.len());
        while let Some(res) = futs.next().await {
            results.push(res?);
        }
        Ok(results)
    }

    /// Is `addr` already labelled as an entity category that legitimately
    /// serves large numbers of unrelated owners (exchange, bridge, DeFi
    /// router, mining pool)? Sanctioned/scam/darknet/gambling entities are
    /// deliberately excluded — those satellite addresses are exactly what
    /// clustering is meant to surface, not infrastructure noise to filter.
    async fn is_shared_infrastructure(&self, addr: &Address) -> DomainResult<bool> {
        let entity = self.entities.find_by_address(addr).await?;
        Ok(matches!(
            entity.map(|e| e.category().clone()),
            Some(
                EntityCategory::Exchange
                    | EntityCategory::Bridge
                    | EntityCategory::DefiProtocol
                    | EntityCategory::Mixer
                    | EntityCategory::Mining
            )
        ))
    }

    /// Is `addr` a smart contract? Used to keep clustering heuristics that
    /// assume a *personal* address (e.g. deposit-reuse) from firing on
    /// contracts (DEX routers, token contracts, pool addresses), which
    /// routinely receive from many unrelated senders by design. Returns
    /// `false` (rather than erroring) when no address-kind repository is
    /// wired up, or when the kind hasn't been classified yet — the caller's
    /// other checks (fan-in caps, entity labels) still apply.
    async fn is_contract(&self, addr: &Address) -> DomainResult<bool> {
        let Some(kinds) = &self.address_kinds else {
            return Ok(false);
        };
        Ok(matches!(kinds.kind(addr).await?, AddressKind::Contract))
    }
}

#[async_trait]
impl<R, E> RiskPort for RiskService<R, E>
where
    R: TransferRepository,
    E: EntityRepository,
{
    #[tracing::instrument(skip(self, req), fields(
        direction = ?req.direction(),
        strategy = ?req.strategy(),
        max_hops = req.limits().max_hops(),
        max_addresses = req.limits().max_addresses(),
    ))]
    async fn trace(&self, req: TraceRequest) -> DomainResult<TraceResult> {
        let cache_key = TraceCacheKey::build(&req);
        if let Some(cached) = self.trace_cache.get(&cache_key).await {
            tracing::debug!("trace cache hit");
            return Ok(cached);
        }
        tracing::info!(
            direction = ?req.direction(),
            strategy = ?req.strategy(),
            max_hops = req.limits().max_hops(),
            max_addresses = req.limits().max_addresses(),
            "trace started"
        );

        let seeds = self.resolve_seeds(req.origin(), req.direction()).await?;
        tracing::debug!(seeds = seeds.len(), "trace seeds resolved");

        let mut paths: Vec<FlowPath> = Vec::new();
        let mut sinks: Vec<Sink> = Vec::new();
        let mut addresses_visited: HashSet<Address> = HashSet::new();
        let mut transfers_evaluated: usize = 0;
        let mut truncated = false;

        let mut out_cache: HashMap<Address, Arc<Vec<Arc<Transfer>>>> = HashMap::new();
        let mut in_cache: HashMap<Address, Arc<Vec<Arc<Transfer>>>> = HashMap::new();

        let mut queue: Vec<(Vec<Arc<Transfer>>, Address, Amount, HashSet<Address>)> = seeds
            .into_iter()
            .map(|t| {
                let (addr, origin_side) = match req.direction() {
                    TraceDirection::Forward | TraceDirection::Both => {
                        (t.to().clone(), t.from().clone())
                    }
                    TraceDirection::Backward => (t.from().clone(), t.to().clone()),
                };
                let amount = t.amount();
                let mut path_visited = HashSet::new();
                path_visited.insert(addr.clone());
                path_visited.insert(origin_side);
                (vec![Arc::new(t)], addr, amount, path_visited)
            })
            .collect();

        while let Some((path, addr, tainted, path_visited)) = queue.pop() {
            if path.len() as u32 > req.limits().max_hops() {
                tracing::debug!(
                    hops = path.len(),
                    max_hops = req.limits().max_hops(),
                    "path truncated: max hops"
                );
                truncated = true;
                continue;
            }
            if addresses_visited.len() >= req.limits().max_addresses() {
                tracing::warn!(
                    addresses = addresses_visited.len(),
                    "trace truncated: max_addresses reached"
                );
                truncated = true;
                break;
            }
            if paths.len() >= req.limits().max_paths() {
                tracing::warn!(paths = paths.len(), "trace truncated: max_paths reached");
                truncated = true;
                break;
            }

            tracing::trace!(address = %crate::addr_hex(&addr), depth = path.len(), "trace visiting");

            addresses_visited.insert(addr.clone());

            let next_arcs = self
                .fetch_cached(&mut out_cache, &mut in_cache, &addr, req.direction(), false)
                .await?;
            transfers_evaluated += next_arcs.len();

            let strategy = req.strategy();
            let is_haircut = matches!(strategy, TaintStrategy::Haircut);
            let is_ordered = matches!(strategy, TaintStrategy::Fifo | TaintStrategy::Lifo);

            let total_in: Amount = if matches!(req.direction(), TraceDirection::Backward) {
                next_arcs
                    .iter()
                    .filter(|t| t.amount().decimals() == tainted.decimals())
                    .fold(Amount::zero(tainted.decimals()), |acc, t| acc + t.amount())
            } else if is_haircut || is_ordered || next_arcs.is_empty() {
                let inc = self
                    .fetch_cached(&mut out_cache, &mut in_cache, &addr, req.direction(), true)
                    .await?;
                inc.iter()
                    .filter(|t| t.amount().decimals() == tainted.decimals())
                    .fold(Amount::zero(tainted.decimals()), |acc, t| acc + t.amount())
            } else {
                tainted
            };

            let taint_ratio: Ratio = if is_haircut {
                if total_in.is_zero() {
                    Ratio::ONE
                } else {
                    tainted.ratio_of(&total_in)
                }
            } else {
                Ratio::ONE
            };

            // Per-edge taint distribution under FIFO/LIFO: pre-compute how much
            // of `tainted` each outgoing edge carries, draining from the
            // chronologically-first edges (FIFO) or last edges (LIFO).
            let ordered_propagation: Option<HashMap<domain::transfer::TransferId, Amount>> =
                if is_ordered {
                    Some(distribute_ordered(
                        &next_arcs,
                        tainted,
                        matches!(strategy, TaintStrategy::Lifo),
                    ))
                } else {
                    None
                };

            if next_arcs.is_empty() {
                let sink = self
                    .classify_sink(&addr, tainted, total_in.max(tainted))
                    .await?;
                sinks.push(sink);
                let depth = path.len() as u32;
                let hops = path.iter().map(|a| (**a).clone()).collect();
                paths.push(FlowPath::new(hops, tainted, taint_ratio, depth));
                continue;
            }

            let context_for_sig: Vec<Transfer> = if req.limits().min_edge_significance().is_some()
            {
                next_arcs.iter().map(|a| (**a).clone()).collect()
            } else {
                Vec::new()
            };

            for t in next_arcs.iter() {
                if !req.include_unconfirmed() && !t.is_confirmed() {
                    continue;
                }

                if let Some(min_sig) = req.limits().min_edge_significance()
                    && edge_significance(t, &context_for_sig) < min_sig
                {
                    continue;
                }

                let propagated = match strategy {
                    TaintStrategy::Poison => t.amount(),
                    TaintStrategy::Haircut => taint_ratio.apply_to(t.amount()),
                    TaintStrategy::Fifo | TaintStrategy::Lifo => ordered_propagation
                        .as_ref()
                        .and_then(|m| m.get(t.id()).copied())
                        .unwrap_or_else(|| Amount::zero(tainted.decimals())),
                };

                if propagated.is_zero() {
                    continue;
                }

                if let Some(min_ratio) = req.limits().min_amount_ratio()
                    && propagated.ratio_of(&t.amount()) < min_ratio
                {
                    continue;
                }

                // For Both: direction follows the edge relative to `addr`,
                // not the global trace direction.
                let next_addr = match req.direction() {
                    TraceDirection::Forward => t.to().clone(),
                    TraceDirection::Backward => t.from().clone(),
                    TraceDirection::Both => {
                        if t.from() == &addr {
                            t.to().clone()
                        } else {
                            t.from().clone()
                        }
                    }
                };

                if path_visited.contains(&next_addr) {
                    continue;
                }

                let mut next_visited = path_visited.clone();
                next_visited.insert(next_addr.clone());
                let mut next_path = path.clone();
                next_path.push(Arc::clone(t));
                queue.push((next_path, next_addr, propagated, next_visited));
            }
        }

        // Dedup by address with max-taint aggregation, then sort.
        let mut by_addr: HashMap<Address, Sink> = HashMap::new();
        for s in sinks.into_iter() {
            match by_addr.get(s.address()) {
                Some(existing)
                    if existing.tainted_amount().raw() >= s.tainted_amount().raw() => {}
                _ => {
                    by_addr.insert(s.address().clone(), s);
                }
            }
        }
        let mut sinks: Vec<Sink> = by_addr.into_values().collect();
        sinks.sort_by_key(|s| std::cmp::Reverse(s.risk_score()));

        let paths_found = paths.len();
        let depth_reached = paths.iter().map(|p| p.depth()).max().unwrap_or(0);

        tracing::info!(
            addresses_visited = addresses_visited.len(),
            transfers_evaluated,
            paths_found,
            sinks = sinks.len(),
            depth_reached,
            truncated,
            "trace complete"
        );

        let result = TraceResult::new(
            req,
            paths,
            sinks,
            TraceStats::new(
                addresses_visited.len(),
                transfers_evaluated,
                paths_found,
                depth_reached,
                truncated,
            ),
        );
        self.trace_cache.insert(cache_key, result.clone()).await;
        Ok(result)
    }

    #[tracing::instrument(skip(self, addr), fields(address = %crate::addr_hex(addr)))]
    async fn score(&self, addr: &Address) -> DomainResult<RiskReport> {
        if let Some(cached) = self.score_cache.get(addr).await {
            tracing::debug!(address = %crate::addr_hex(addr), "score cache hit");
            return Ok(cached);
        }
        tracing::info!(address = %crate::addr_hex(addr), "score started");
        let mut signals: Vec<RiskSignal> = Vec::new();

        if let Some(entity) = self.entities.find_by_address(addr).await? {
            tracing::debug!(
                address = %crate::addr_hex(addr),
                category = ?entity.category(),
                "direct entity label found"
            );
            let (severity, kind) = match entity.category() {
                EntityCategory::Sanctioned { .. } => {
                    (RiskScore::CRITICAL, RiskSignalKind::SanctionedCounterparty)
                }
                EntityCategory::Mixer => (RiskScore::HIGH, RiskSignalKind::MixerInteraction),
                EntityCategory::Darknet => (RiskScore::CRITICAL, RiskSignalKind::DarknetMarket),
                EntityCategory::Scam => (RiskScore::HIGH, RiskSignalKind::DirectExposure),
                _ => (RiskScore::LOW, RiskSignalKind::DirectExposure),
            };
            signals.push(RiskSignal::new(
                kind,
                severity,
                format!(
                    "Address is labelled: {}",
                    entity.label().map(|l| l.name()).unwrap_or("unknown")
                ),
                RiskEvidence::EntityCategory(entity.category().clone()),
            ));
        }

        // Internal trace knobs are now config-driven so operators can tune
        // depth/breadth without rebuilding.
        let limits = TraceLimits::new(
            self.score_cfg.trace_max_depth,
            self.score_cfg.trace_max_nodes,
            self.score_cfg.trace_max_paths,
            Some(Ratio::from_percent(self.score_cfg.trace_min_amount_ratio_percent)),
        );
        let backward = self
            .trace(TraceRequest::new(
                TraceOrigin::Address(addr.clone()),
                TraceDirection::Backward,
                TaintStrategy::Haircut,
                limits,
                false,
            ))
            .await?;

        for sink in backward.terminal_sinks() {
            if sink.risk_score() >= self.score_cfg.sink_severity_threshold {
                let hops = backward
                    .paths()
                    .iter()
                    .filter(|p| p.destination() == Some(sink.address()))
                    .map(|p| p.depth())
                    .min()
                    .unwrap_or(0);

                let kind = if hops == 0 {
                    RiskSignalKind::DirectExposure
                } else {
                    RiskSignalKind::IndirectExposure { hops }
                };

                signals.push(RiskSignal::new(
                    kind,
                    RiskScore::new(sink.risk_score()),
                    format!(
                        "Funds traceable to high-risk sink ({})",
                        sink_label(sink.kind())
                    ),
                    RiskEvidence::SinkExposure(vec![sink.clone()]),
                ));
            }
        }

        let forward = self
            .trace(TraceRequest::new(
                TraceOrigin::Address(addr.clone()),
                TraceDirection::Forward,
                TaintStrategy::Haircut,
                limits,
                false,
            ))
            .await?;

        for sink in forward.terminal_sinks() {
            if sink.risk_score() >= self.score_cfg.sink_severity_threshold {
                signals.push(RiskSignal::new(
                    RiskSignalKind::DirectExposure,
                    RiskScore::new(sink.risk_score()),
                    format!(
                        "Funds sent to high-risk destination ({})",
                        sink_label(sink.kind())
                    ),
                    RiskEvidence::SinkExposure(vec![sink.clone()]),
                ));
            }
        }

        // Config-driven aggregation: dedup signals by (kind, target), then
        // either `Max` or `WeightedCount`. `with_score` skips the legacy
        // max-aggregator in `RiskReport::new` since we've already computed
        // the right number ourselves.
        let overall = self.score_cfg.aggregate(&signals);
        let unique_signals = self.score_cfg.unique_count(&signals);
        let report = RiskReport::with_score(addr.clone(), signals, overall);
        tracing::info!(
            address = %crate::addr_hex(addr),
            score = report.overall_score().value(),
            raw_signals = report.signals().len(),
            unique_signals,
            aggregation = ?self.score_cfg.aggregation,
            is_high_risk = report.is_high_risk(),
            "score complete"
        );
        self.score_cache.insert(addr.clone(), report.clone()).await;
        Ok(report)
    }

    #[tracing::instrument(skip(self, addr), fields(address = %crate::addr_hex(addr)))]
    async fn check_sanctions(&self, addr: &Address) -> DomainResult<SanctionsCheckResult> {
        if let Some(cached) = self.sanctions_cache.get(addr).await {
            tracing::debug!(address = %crate::addr_hex(addr), "sanctions cache hit");
            return Ok(cached);
        }
        tracing::debug!(address = %crate::addr_hex(addr), "sanctions check");
        let entity = self.entities.find_by_address(addr).await?;

        let (is_sanctioned, sanction_list, label) = match entity {
            Some(e) => {
                let list: Option<SanctionList> =
                    if let EntityCategory::Sanctioned { sanction_list } = e.category() {
                        Some(sanction_list.clone())
                    } else {
                        None
                    };
                let label = e.label().map(|l| l.name().to_string());
                (list.is_some(), list, label)
            }
            None => (false, None, None),
        };

        tracing::debug!(
            address = %crate::addr_hex(addr),
            is_sanctioned,
            "sanctions check result"
        );
        let result = SanctionsCheckResult::new(addr.clone(), is_sanctioned, sanction_list, label);
        self.sanctions_cache
            .insert(addr.clone(), result.clone())
            .await;
        Ok(result)
    }

    async fn check_sanctions_batch(
        &self,
        addrs: &[Address],
    ) -> DomainResult<Vec<SanctionsCheckResult>> {
        let mut results = Vec::with_capacity(addrs.len());
        for addr in addrs {
            results.push(self.check_sanctions(addr).await?);
        }
        Ok(results)
    }

    async fn deposit_reuse_cluster(
        &self,
        deposit_addr: &Address,
    ) -> DomainResult<Option<ClusterEvidence>> {
        // A real per-customer deposit address is rarely visited by more than
        // a handful of unrelated senders. An address already labelled as
        // shared infrastructure (exchange, bridge, DeFi router, miner), or
        // that is itself a contract (DEX router, token, pool — all routinely
        // receive from countless unrelated owners by design), is the
        // opposite case: "shared senders" proves nothing about common
        // ownership there and must not become a merge point.
        if self.is_shared_infrastructure(deposit_addr).await? || self.is_contract(deposit_addr).await? {
            return Ok(None);
        }

        let incoming = self.transfers.find_incoming(deposit_addr, None).await?;

        if incoming.len() < self.heuristics.deposit_reuse_min_incoming {
            return Ok(None);
        }

        let senders: Vec<Address> = {
            let mut v: Vec<Address> = incoming.into_iter().map(|t| t.from().clone()).collect();
            v.sort_by(|a, b| a.bytes().cmp(b.bytes()));
            v.dedup();
            v
        };

        // Below the configured floor, nothing to reuse. Above max_fanin the
        // address itself behaves like a hub by plain sender count, even
        // without a label — same reasoning as the fan-in/fan-out cap.
        if senders.len() < self.heuristics.deposit_reuse_min_senders
            || senders.len() > self.heuristics.max_fanin
        {
            return Ok(None);
        }

        Ok(Some(ClusterEvidence::new(
            senders,
            ClusteringHeuristic::DepositAddressReuse,
            Confidence::MEDIUM,
            Some(format!(
                "All senders route to deposit address {}",
                deposit_addr
            )),
        )))
    }

    async fn detect_peeling_chain(&self, addr: &Address) -> DomainResult<Option<ClusterEvidence>> {
        let incoming = self.transfers.find_incoming(addr, None).await?;
        let outgoing = self.transfers.find_outgoing(addr, None).await?;

        if incoming.is_empty() || outgoing.is_empty() {
            return Ok(None);
        }

        // A peeling chain is a per-asset phenomenon: an address that
        // received USDC and sent ETH isn't peeling, it's just mixing
        // activity. `Amount` arithmetic asserts equal decimals (panics on
        // 18-vs-6 ETH/USDC mixes), so we MUST group by asset before any
        // summation.
        use domain::asset::AssetId;
        let mut in_by_asset: HashMap<AssetId, Amount> = HashMap::new();
        for t in &incoming {
            let amt = t.amount();
            in_by_asset
                .entry(t.asset().clone())
                .and_modify(|a| *a = *a + amt)
                .or_insert(amt);
        }
        let mut out_by_asset: HashMap<AssetId, Amount> = HashMap::new();
        for t in &outgoing {
            let amt = t.amount();
            out_by_asset
                .entry(t.asset().clone())
                .and_modify(|a| *a = *a + amt)
                .or_insert(amt);
        }

        // Pick the asset with the strongest peeling signal — the smallest
        // retained ratio that's still under the 5% threshold. Assets that
        // either don't appear in outgoing, or have out > in, are skipped.
        let mut best: Option<(Ratio, AssetId)> = None;
        for (asset, in_total) in &in_by_asset {
            let Some(out_total) = out_by_asset.get(asset) else {
                continue;
            };
            if out_total.raw() > in_total.raw() {
                continue;
            }
            let retained = *in_total - *out_total;
            let retained_ratio = retained.ratio_of(in_total);
            if retained_ratio > Ratio::from_percent(5) {
                continue;
            }
            best = match best {
                None => Some((retained_ratio, asset.clone())),
                Some((curr, _)) if retained_ratio < curr => Some((retained_ratio, asset.clone())),
                other => other,
            };
        }

        let Some((retained_ratio, best_asset)) = best else {
            return Ok(None);
        };

        // Only the counterparties of the peeling asset's own outgoing legs
        // belong in the evidence — an address that received a different,
        // non-peeling asset from `addr` didn't participate in this pattern.
        let chain_addrs: Vec<Address> = outgoing
            .into_iter()
            .filter(|t| t.asset() == &best_asset)
            .map(|t| t.to().clone())
            .collect();

        Ok(Some(ClusterEvidence::new(
            chain_addrs,
            ClusteringHeuristic::PeelingChain,
            Confidence::HIGH,
            Some(format!(
                "Address retains only {:.1}% of inflow",
                retained_ratio.as_f64() * 100.0
            )),
        )))
    }

    async fn detect_temporal_burst(
        &self,
        addr: &Address,
    ) -> DomainResult<Option<ClusterEvidence>> {
        let cfg = self.heuristics;
        let mut all: Vec<Transfer> = self.transfers.find_outgoing(addr, None).await?;
        all.extend(self.transfers.find_incoming(addr, None).await?);
        if all.len() < cfg.burst_min_count {
            return Ok(None);
        }
        let window_secs = cfg.burst_window.as_secs() as i64;
        if window_secs == 0 {
            return Ok(None);
        }

        // Bucket transfers by floor(ts / window).
        let mut counts: HashMap<i64, usize> = HashMap::new();
        let mut bucket_addrs: HashMap<i64, HashSet<Address>> = HashMap::new();
        for t in all.iter() {
            let bucket = t.timestamp().timestamp() / window_secs;
            *counts.entry(bucket).or_insert(0) += 1;
            let counter = if t.from() == addr {
                t.to().clone()
            } else {
                t.from().clone()
            };
            bucket_addrs.entry(bucket).or_default().insert(counter);
        }

        // Baseline = median over all populated buckets except the maximum one.
        let mut buckets: Vec<(i64, usize)> = counts.iter().map(|(k, v)| (*k, *v)).collect();
        buckets.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
        let (peak_bucket, peak_count) = buckets[0];

        let baseline = if buckets.len() < 2 {
            0.0
        } else {
            let mut tail: Vec<usize> = buckets.iter().skip(1).map(|(_, c)| *c).collect();
            tail.sort();
            let mid = tail.len() / 2;
            tail[mid] as f64
        };

        if (peak_count as f64) < cfg.burst_min_count as f64 {
            return Ok(None);
        }
        if baseline > 0.0 && (peak_count as f64) < baseline * cfg.burst_multiplier {
            return Ok(None);
        }

        let mut addrs: Vec<Address> = bucket_addrs
            .remove(&peak_bucket)
            .map(|s| s.into_iter().collect())
            .unwrap_or_default();
        addrs.sort_by(|a, b| a.bytes().cmp(b.bytes()));

        Ok(Some(ClusterEvidence::new(
            addrs,
            ClusteringHeuristic::TemporalBurst,
            Confidence::MEDIUM,
            Some(format!(
                "Burst of {peak_count} transfers in a {window_secs}s window (baseline median {baseline:.1}) around {addr}"
            )),
        )))
    }

    async fn detect_fixed_amount_clustering(
        &self,
        addr: &Address,
    ) -> DomainResult<Option<ClusterEvidence>> {
        let cfg = self.heuristics;
        let mut all: Vec<Transfer> = self.transfers.find_outgoing(addr, None).await?;
        all.extend(self.transfers.find_incoming(addr, None).await?);
        if all.len() < cfg.fixed_amount_min_count {
            return Ok(None);
        }
        if cfg.fixed_amount_bucket_usd <= 0.0 {
            return Ok(None);
        }

        // Bucket by rounded USD when available; fall back to raw amount if not.
        let mut buckets: HashMap<i64, Vec<Transfer>> = HashMap::new();
        for t in all.iter() {
            let bucket: i64 = if let Some(usd) = t.usd_value() {
                (usd.value() / cfg.fixed_amount_bucket_usd).round() as i64
            } else {
                // Raw u128 lossy bucket — same asset is usually present, so
                // identical raw amounts cluster together.
                use std::collections::hash_map::DefaultHasher;
                use std::hash::{Hash, Hasher};
                let mut h = DefaultHasher::new();
                t.amount().raw().to_string().hash(&mut h);
                t.asset().hash(&mut h);
                h.finish() as i64
            };
            buckets.entry(bucket).or_default().push(t.clone());
        }

        let mut hits: Vec<(i64, Vec<Transfer>)> = buckets
            .into_iter()
            .filter(|(_, v)| v.len() >= cfg.fixed_amount_min_count)
            .collect();
        if hits.is_empty() {
            return Ok(None);
        }
        hits.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));
        let (_bucket, transfers) = hits.into_iter().next().unwrap();
        let count = transfers.len();
        let mut addrs: HashSet<Address> = HashSet::new();
        for t in transfers.iter() {
            addrs.insert(t.from().clone());
            addrs.insert(t.to().clone());
        }
        addrs.remove(addr);
        let mut addrs: Vec<Address> = addrs.into_iter().collect();
        addrs.sort_by(|a, b| a.bytes().cmp(b.bytes()));

        Ok(Some(ClusterEvidence::new(
            addrs,
            ClusteringHeuristic::FixedAmountClustering,
            Confidence::MEDIUM,
            Some(format!(
                "{count} transfers cluster into the same ~${} bucket around {addr}",
                cfg.fixed_amount_bucket_usd as i64
            )),
        )))
    }

    async fn detect_dwell_time(
        &self,
        addr: &Address,
    ) -> DomainResult<Option<ClusterEvidence>> {
        let cfg = self.heuristics;
        let mut incoming = self.transfers.find_incoming(addr, None).await?;
        let mut outgoing = self.transfers.find_outgoing(addr, None).await?;
        incoming.sort_by_key(|t| t.timestamp());
        outgoing.sort_by_key(|t| t.timestamp());

        if outgoing.len() < cfg.dwell_min_pairs {
            return Ok(None);
        }

        // For each out tx, find the latest in tx with ts <= out.ts. Record the
        // dwell delta in seconds. Two-pointer over already-sorted vecs.
        let mut deltas: Vec<i64> = Vec::new();
        let mut counterparties: HashSet<Address> = HashSet::new();
        let mut i = 0usize;
        let mut latest_in_ts: Option<chrono::DateTime<chrono::Utc>> = None;
        for out in outgoing.iter() {
            while i < incoming.len() && incoming[i].timestamp() <= out.timestamp() {
                latest_in_ts = Some(incoming[i].timestamp());
                i += 1;
            }
            if let Some(in_ts) = latest_in_ts {
                let delta = (out.timestamp() - in_ts).num_seconds();
                if delta >= 0 {
                    deltas.push(delta);
                    counterparties.insert(out.to().clone());
                }
            }
        }

        if deltas.len() < cfg.dwell_min_pairs {
            return Ok(None);
        }
        deltas.sort();
        let median_delta = deltas[deltas.len() / 2];
        if (median_delta as u64) > cfg.dwell_max_secs {
            return Ok(None);
        }

        let count = deltas.len();
        let mut addrs: Vec<Address> = counterparties.into_iter().collect();
        addrs.sort_by(|a, b| a.bytes().cmp(b.bytes()));

        Ok(Some(ClusterEvidence::new(
            addrs,
            ClusteringHeuristic::DwellTimePassThrough,
            Confidence::MEDIUM,
            Some(format!(
                "Median dwell time {median_delta}s across {count} in/out matched pairs (cap {}s) — pass-through pattern at {addr}",
                cfg.dwell_max_secs
            )),
        )))
    }

    async fn cluster_address(
        &self,
        addr: &Address,
    ) -> DomainResult<Vec<Vec<Address>>> {
        let mut uf = UnionFind::new();
        uf.insert(addr.clone());

        // Defense in depth: even with per-detector fanin/fanout caps, a
        // labelled hub can slip into evidence produced by a different
        // heuristic (e.g. temporal_burst, fixed_amount_clustering). Never
        // let a known shared-infrastructure address act as a merge point —
        // drop it from the group instead of unioning through it. Memoized
        // locally since the same counterparty can recur across heuristics.
        let mut hub_cache: HashMap<Address, bool> = HashMap::new();

        for ev in [
            self.detect_fan_out(addr).await?,
            self.detect_fan_in(addr).await?,
            self.detect_smurfing_cycle(addr).await?,
            self.detect_temporal_burst(addr).await?,
            self.detect_fixed_amount_clustering(addr).await?,
            self.detect_dwell_time(addr).await?,
            self.deposit_reuse_cluster(addr).await?,
            self.detect_peeling_chain(addr).await?,
        ]
        .into_iter()
        .flatten()
        {
            let mut group: Vec<Address> = Vec::with_capacity(ev.addresses().len() + 1);
            for a in ev.addresses() {
                let is_hub = match hub_cache.get(a) {
                    Some(v) => *v,
                    None => {
                        let v = self.is_shared_infrastructure(a).await?;
                        hub_cache.insert(a.clone(), v);
                        v
                    }
                };
                if !is_hub {
                    group.push(a.clone());
                }
            }
            group.push(addr.clone());
            if group.len() < 2 {
                continue;
            }
            uf.union_all(&group);
        }

        Ok(uf.components())
    }

    async fn detect_fan_out(&self, addr: &Address) -> DomainResult<Option<ClusterEvidence>> {
        let outgoing = self.transfers.find_outgoing(addr, None).await?;
        let cfg = self.heuristics;
        Ok(burst_window_evidence(
            &outgoing,
            |t| t.to().clone(),
            cfg.min_fanout,
            cfg.max_fanout,
            cfg.fan_window,
            ClusteringHeuristic::FanOut,
            |n, window_secs| {
                format!("{n} unique receivers within a {window_secs}s window from {addr}")
            },
        ))
    }

    async fn detect_fan_in(&self, addr: &Address) -> DomainResult<Option<ClusterEvidence>> {
        let incoming = self.transfers.find_incoming(addr, None).await?;
        let cfg = self.heuristics;
        Ok(burst_window_evidence(
            &incoming,
            |t| t.from().clone(),
            cfg.min_fanin,
            cfg.max_fanin,
            cfg.fan_window,
            ClusteringHeuristic::FanIn,
            |n, window_secs| {
                format!("{n} unique senders within a {window_secs}s window into {addr}")
            },
        ))
    }

    async fn detect_smurfing_cycle(&self, addr: &Address) -> DomainResult<Option<ClusterEvidence>> {
        let cfg = self.heuristics;
        let outgoing = self.transfers.find_outgoing(addr, None).await?;

        let receivers: Vec<(Address, chrono::DateTime<chrono::Utc>)> = {
            let mut v: Vec<(Address, chrono::DateTime<chrono::Utc>)> = outgoing
                .iter()
                .map(|t| (t.to().clone(), t.timestamp()))
                .collect();
            v.sort_by(|a, b| a.0.bytes().cmp(b.0.bytes()).then(a.1.cmp(&b.1)));
            v.dedup_by(|a, b| a.0 == b.0);
            v.retain(|(r, _)| r != addr);
            v
        };

        // Below min_fanout there's no smurf-style spread; above max_fanout
        // `addr` is already behaving like shared infrastructure (payroll,
        // airdrop, exchange payout) rather than a distributor structuring
        // funds, so don't try to trace convergence from it.
        if receivers.len() < cfg.min_fanout || receivers.len() > cfg.max_fanout {
            return Ok(None);
        }

        let window_chrono = chrono::Duration::from_std(cfg.smurf_window)
            .unwrap_or_else(|_| chrono::Duration::seconds(86_400));

        let mut outgoing_cache: HashMap<Address, Vec<Transfer>> = HashMap::new();
        let mut downstream: HashMap<Address, HashSet<Address>> = HashMap::new();

        for (r, r_ts) in receivers.iter() {
            let reached = bfs_downstream(
                &self.transfers,
                &mut outgoing_cache,
                r,
                *r_ts,
                window_chrono,
                cfg.smurf_max_depth,
            )
            .await?;
            for y in reached {
                if &y == addr || &y == r {
                    continue;
                }
                downstream.entry(y).or_default().insert(r.clone());
            }
        }

        // Same reasoning on the cash-out side: a `y` that absorbs more than
        // max_fanin intermediaries is a popular sink (exchange deposit,
        // bridge) rather than a genuine convergence point for this specific
        // distributor, so it's excluded rather than treated as evidence.
        let mut hits: Vec<(Address, HashSet<Address>)> = downstream
            .into_iter()
            .filter(|(_, v)| v.len() >= cfg.min_fanin && v.len() <= cfg.max_fanin)
            .collect();
        if hits.is_empty() {
            return Ok(None);
        }

        hits.sort_by(|a, b| {
            b.1.len()
                .cmp(&a.1.len())
                .then_with(|| a.0.bytes().cmp(b.0.bytes()))
        });
        let (y, via_set) = hits.into_iter().next().unwrap();
        let mut via: Vec<Address> = via_set.into_iter().collect();
        via.sort_by(|a, b| a.bytes().cmp(b.bytes()));
        let via_count = via.len();

        let mut addresses: Vec<Address> = Vec::with_capacity(2 + via.len());
        addresses.push(addr.clone());
        addresses.extend(via);
        addresses.push(y.clone());

        Ok(Some(ClusterEvidence::new(
            addresses,
            ClusteringHeuristic::SmurfingCycle,
            Confidence::MEDIUM,
            Some(format!(
                "{via_count} intermediaries route from {addr} to cash-out {y} within {}s (depth ≤ {})",
                cfg.smurf_window.as_secs(),
                cfg.smurf_max_depth
            )),
        )))
    }

}

impl<R, E> RiskService<R, E>
where
    R: TransferRepository,
    E: EntityRepository,
{
    async fn fetch_cached(
        &self,
        out_cache: &mut HashMap<Address, Arc<Vec<Arc<Transfer>>>>,
        in_cache: &mut HashMap<Address, Arc<Vec<Arc<Transfer>>>>,
        addr: &Address,
        direction: TraceDirection,
        force_incoming: bool,
    ) -> DomainResult<Arc<Vec<Arc<Transfer>>>> {
        if force_incoming {
            if let Some(v) = in_cache.get(addr) {
                return Ok(Arc::clone(v));
            }
            let raw = self.transfers.find_incoming(addr, None).await?;
            let arcs = Arc::new(raw.into_iter().map(Arc::new).collect::<Vec<_>>());
            in_cache.insert(addr.clone(), Arc::clone(&arcs));
            return Ok(arcs);
        }

        match direction {
            TraceDirection::Forward => {
                if let Some(v) = out_cache.get(addr) {
                    return Ok(Arc::clone(v));
                }
                let raw = self.transfers.find_outgoing(addr, None).await?;
                let arcs = Arc::new(raw.into_iter().map(Arc::new).collect::<Vec<_>>());
                out_cache.insert(addr.clone(), Arc::clone(&arcs));
                Ok(arcs)
            }
            TraceDirection::Backward => {
                if let Some(v) = in_cache.get(addr) {
                    return Ok(Arc::clone(v));
                }
                let raw = self.transfers.find_incoming(addr, None).await?;
                let arcs = Arc::new(raw.into_iter().map(Arc::new).collect::<Vec<_>>());
                in_cache.insert(addr.clone(), Arc::clone(&arcs));
                Ok(arcs)
            }
            TraceDirection::Both => {
                let out = if let Some(v) = out_cache.get(addr) {
                    Arc::clone(v)
                } else {
                    let raw = self.transfers.find_outgoing(addr, None).await?;
                    let arcs = Arc::new(raw.into_iter().map(Arc::new).collect::<Vec<_>>());
                    out_cache.insert(addr.clone(), Arc::clone(&arcs));
                    arcs
                };
                let inc = if let Some(v) = in_cache.get(addr) {
                    Arc::clone(v)
                } else {
                    let raw = self.transfers.find_incoming(addr, None).await?;
                    let arcs = Arc::new(raw.into_iter().map(Arc::new).collect::<Vec<_>>());
                    in_cache.insert(addr.clone(), Arc::clone(&arcs));
                    arcs
                };
                let mut combined = Vec::with_capacity(out.len() + inc.len());
                combined.extend(out.iter().map(Arc::clone));
                combined.extend(inc.iter().map(Arc::clone));
                Ok(Arc::new(combined))
            }
        }
    }

    async fn resolve_seeds(
        &self,
        origin: &TraceOrigin,
        direction: TraceDirection,
    ) -> DomainResult<Vec<Transfer>> {
        match origin {
            TraceOrigin::Address(addr) => match direction {
                TraceDirection::Backward => self.transfers.find_incoming(addr, None).await,
                TraceDirection::Forward | TraceDirection::Both => {
                    self.transfers.find_outgoing(addr, None).await
                }
            },
            TraceOrigin::Transaction { chain, hash } => {
                self.transfers.find_by_tx(*chain, hash).await
            }
            TraceOrigin::Transfer(id) => {
                let all = self.transfers.find_by_tx(id.chain(), id.tx_hash()).await?;
                Ok(all
                    .into_iter()
                    .filter(|t| t.id().index() == id.index())
                    .collect())
            }
        }
    }

    async fn classify_sink(
        &self,
        addr: &Address,
        tainted: Amount,
        total: Amount,
    ) -> DomainResult<Sink> {
        let ratio = tainted.ratio_of(&total);

        let kind = match self.entities.find_by_address(addr).await? {
            Some(entity) => match entity.category() {
                EntityCategory::Exchange => SinkKind::Exchange {
                    name: entity
                        .label()
                        .map(|l| l.name().to_string())
                        .unwrap_or_default(),
                    requires_subpoena: true,
                },
                EntityCategory::Bridge => SinkKind::Bridge {
                    destination_chain: None,
                },
                EntityCategory::Mixer => SinkKind::Mixer,
                EntityCategory::Sanctioned { .. } => SinkKind::Sanctioned,
                EntityCategory::Darknet => SinkKind::Darknet,
                _ => SinkKind::Unresolved,
            },
            None => SinkKind::Unresolved,
        };

        Ok(Sink::new(addr.clone(), kind, tainted, ratio))
    }
}

/// Slide a time-window over `transfers` (sorted by timestamp) and return
/// evidence if any window contains at least `min_unique` distinct counterparties
/// produced by `counterparty`. Returns the maximal-coverage window.
fn burst_window_evidence<F, N>(
    transfers: &[Transfer],
    counterparty: F,
    min_unique: usize,
    max_unique: usize,
    window: Duration,
    heuristic: ClusteringHeuristic,
    note: N,
) -> Option<ClusterEvidence>
where
    F: Fn(&Transfer) -> Address,
    N: Fn(usize, u64) -> String,
{
    if transfers.len() < min_unique {
        return None;
    }

    let mut sorted: Vec<&Transfer> = transfers.iter().collect();
    sorted.sort_by_key(|t| t.timestamp());

    let window_chrono = chrono::Duration::from_std(window).ok()?;

    let mut best: Option<Vec<Address>> = None;
    let mut left = 0usize;

    for right in 0..sorted.len() {
        let right_ts = sorted[right].timestamp();
        while left < right && right_ts - sorted[left].timestamp() > window_chrono {
            left += 1;
        }

        let mut seen: HashSet<Address> = HashSet::new();
        let mut uniq: Vec<Address> = Vec::new();
        for t in &sorted[left..=right] {
            let cp = counterparty(t);
            if seen.insert(cp.clone()) {
                uniq.push(cp);
            }
        }

        if uniq.len() >= min_unique && best.as_ref().map(|b| b.len() < uniq.len()).unwrap_or(true) {
            best = Some(uniq);
        }
    }

    let addresses = best?;
    // Above max_unique the address is behaving like shared infrastructure
    // (an exchange hot wallet, a router, a popular contract) rather than a
    // single owner's burst. Reject the whole match rather than truncating
    // it to the cap — a truncated subset would still wrongly union a chunk
    // of unrelated counterparties together.
    if addresses.len() > max_unique {
        return None;
    }
    let n = addresses.len();
    Some(ClusterEvidence::new(
        addresses,
        heuristic,
        Confidence::MEDIUM,
        Some(note(n, window.as_secs())),
    ))
}

/// Heuristic 0..1 score capturing how "load-bearing" an edge is for forensics.
/// Combines:
/// * repetition: ratio of edges between the same (from, to) pair
/// * diversity: distinct counterparties on either side normalised by transfers
/// * USD weight: log-scaled USD value when available
/// * round-number bonus: matches structured-payment pattern
/// `context` is a list of transfers used to compute the local stats — usually
/// all transfers touching either endpoint.
pub fn edge_significance(t: &domain::transfer::Transfer, context: &[domain::transfer::Transfer]) -> f64 {
    if context.is_empty() {
        return 0.0;
    }
    let pair_count = context
        .iter()
        .filter(|c| c.from() == t.from() && c.to() == t.to())
        .count() as f64;
    let repetition = (pair_count / context.len() as f64).min(1.0);

    let mut counterparties: std::collections::HashSet<&domain::primitives::Address> =
        std::collections::HashSet::new();
    for c in context {
        if c.from() == t.from() {
            counterparties.insert(c.to());
        }
        if c.to() == t.to() {
            counterparties.insert(c.from());
        }
    }
    let diversity = 1.0 / (1.0 + counterparties.len() as f64);

    let usd_weight = t
        .usd_value()
        .map(|v| (v.value().max(1.0).ln() / 12.0).clamp(0.0, 1.0))
        .unwrap_or(0.3);

    let amount_str = t.amount().raw().to_string();
    let trailing_zeros = amount_str.chars().rev().take_while(|c| *c == '0').count() as f64;
    let round_bonus = (trailing_zeros / 6.0).clamp(0.0, 1.0);

    (0.30 * repetition + 0.20 * diversity + 0.35 * usd_weight + 0.15 * round_bonus).clamp(0.0, 1.0)
}

/// Pre-compute per-edge taint amounts under FIFO (default) or LIFO
/// (`reverse=true`). Edges are ordered by timestamp; we drain `tainted` from
/// the front (FIFO) or back (LIFO), each edge taking `min(remaining, t.amount())`.
fn distribute_ordered(
    arcs: &[Arc<Transfer>],
    tainted: Amount,
    reverse: bool,
) -> HashMap<domain::transfer::TransferId, Amount> {
    let decimals = tainted.decimals();
    let zero = Amount::zero(decimals);
    let mut map: HashMap<domain::transfer::TransferId, Amount> = HashMap::new();

    let mut order: Vec<&Arc<Transfer>> = arcs
        .iter()
        .filter(|t| t.amount().decimals() == decimals)
        .collect();
    order.sort_by_key(|t| t.timestamp());
    if reverse {
        order.reverse();
    }

    let mut remaining = tainted;
    for t in order {
        if remaining.is_zero() {
            map.insert(t.id().clone(), zero);
            continue;
        }
        let take = if remaining.raw() <= t.amount().raw() {
            remaining
        } else {
            t.amount()
        };
        map.insert(t.id().clone(), take);
        remaining = if remaining.raw() > take.raw() {
            Amount::new(remaining.raw() - take.raw(), decimals)
        } else {
            zero
        };
    }

    map
}

/// BFS downstream from `start` over outgoing transfers, expanding only edges
/// whose timestamp is in `[anchor_ts, anchor_ts + window]`. Returns the set of
/// addresses reachable within `max_depth` hops, not including `start`.
/// Outgoing lookups are memoized into `cache` across calls.
async fn bfs_downstream<R: TransferRepository + ?Sized>(
    repo: &R,
    cache: &mut HashMap<Address, Vec<Transfer>>,
    start: &Address,
    anchor_ts: chrono::DateTime<chrono::Utc>,
    window: chrono::Duration,
    max_depth: u32,
) -> DomainResult<HashSet<Address>> {
    let mut reached: HashSet<Address> = HashSet::new();
    if max_depth == 0 {
        return Ok(reached);
    }

    let mut queue: std::collections::VecDeque<(Address, u32)> = std::collections::VecDeque::new();
    queue.push_back((start.clone(), 0));
    let mut visited: HashSet<Address> = HashSet::new();
    visited.insert(start.clone());

    let upper = anchor_ts + window;

    while let Some((addr, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        if !cache.contains_key(&addr) {
            let outs = repo.find_outgoing(&addr, None).await?;
            cache.insert(addr.clone(), outs);
        }
        let outs = cache.get(&addr).expect("just inserted");
        for t in outs {
            let ts = t.timestamp();
            if ts < anchor_ts || ts > upper {
                continue;
            }
            let next = t.to().clone();
            if visited.insert(next.clone()) {
                reached.insert(next.clone());
                queue.push_back((next, depth + 1));
            }
        }
    }

    Ok(reached)
}

fn sink_label(kind: &SinkKind) -> &'static str {
    match kind {
        SinkKind::Exchange { .. } => "exchange",
        SinkKind::Bridge { .. } => "bridge",
        SinkKind::Mixer => "mixer",
        SinkKind::Sanctioned => "sanctioned",
        SinkKind::Darknet => "darknet",
        SinkKind::Unresolved => "unresolved",
    }
}

#[cfg(test)]
mod heuristics_tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use domain::asset::AssetId;
    use domain::chain::ChainId;
    use domain::entity::{Entity, EntityId};
    use domain::primitives::{Amount, BlockRef, TxRef, U256};
    use domain::transfer::{Finality, TransferId, TransferKind};
    use std::sync::Mutex;

    fn addr(seed: u8) -> Address {
        let mut bytes = vec![0u8; 20];
        bytes[19] = seed;
        Address::new(ChainId::ETH, bytes)
    }

    fn make_transfer(from: &Address, to: &Address, ts_secs: i64, idx: u32) -> Transfer {
        let chain = ChainId::ETH;
        let mut hash = [0u8; 32];
        hash[28..32].copy_from_slice(&idx.to_be_bytes());
        Transfer::new(
            TransferId::new(chain, hash, idx),
            chain,
            TxRef::new(chain, hash),
            from.clone(),
            to.clone(),
            AssetId::native(chain),
            Amount::new(U256::from(1u64), 18),
            BlockRef::new(chain, 100 + idx as u64, [0u8; 32]),
            Utc.timestamp_opt(ts_secs, 0).unwrap(),
            TransferKind::Native,
            Finality::Confirmed,
        )
    }

    #[derive(Default)]
    struct MockTransfers {
        outgoing: Mutex<HashMap<Address, Vec<Transfer>>>,
        incoming: Mutex<HashMap<Address, Vec<Transfer>>>,
    }

    impl MockTransfers {
        fn push(&self, t: Transfer) {
            self.outgoing
                .lock()
                .unwrap()
                .entry(t.from().clone())
                .or_default()
                .push(t.clone());
            self.incoming
                .lock()
                .unwrap()
                .entry(t.to().clone())
                .or_default()
                .push(t);
        }
    }

    #[async_trait]
    impl TransferRepository for MockTransfers {
        async fn save(&self, _transfers: &[Transfer]) -> DomainResult<()> {
            Ok(())
        }
        async fn find_by_address(
            &self,
            _addr: &Address,
            _range: Option<domain::ports::BlockRange>,
            _after: Option<domain::ports::TransferCursor>,
            _limit: usize,
        ) -> DomainResult<Vec<Transfer>> {
            Ok(Vec::new())
        }
        async fn find_by_tx(
            &self,
            _chain: ChainId,
            _tx_hash: &[u8; 32],
        ) -> DomainResult<Vec<Transfer>> {
            Ok(Vec::new())
        }
        async fn find_outgoing(
            &self,
            addr: &Address,
            _after: Option<chrono::DateTime<Utc>>,
        ) -> DomainResult<Vec<Transfer>> {
            Ok(self
                .outgoing
                .lock()
                .unwrap()
                .get(addr)
                .cloned()
                .unwrap_or_default())
        }
        async fn find_incoming(
            &self,
            addr: &Address,
            _after: Option<chrono::DateTime<Utc>>,
        ) -> DomainResult<Vec<Transfer>> {
            Ok(self
                .incoming
                .lock()
                .unwrap()
                .get(addr)
                .cloned()
                .unwrap_or_default())
        }
        async fn min_block_height(&self, _addr: &Address) -> DomainResult<Option<u64>> {
            Ok(None)
        }
        async fn max_block_height(&self, _addr: &Address) -> DomainResult<Option<u64>> {
            Ok(None)
        }
        async fn delete_in_range(
            &self,
            _addr: &Address,
            _from_block: u64,
            _to_block: u64,
        ) -> DomainResult<u64> {
            Ok(0)
        }
    }

    #[derive(Default)]
    struct NopEntities;

    #[async_trait]
    impl EntityRepository for NopEntities {
        async fn find_by_id(&self, _id: &EntityId) -> DomainResult<Option<Entity>> {
            Ok(None)
        }
        async fn find_by_address(&self, _addr: &Address) -> DomainResult<Option<Entity>> {
            Ok(None)
        }
        async fn find_by_label(
            &self,
            _category: &domain::entity::EntityCategory,
            _label_name: &str,
        ) -> DomainResult<Option<Entity>> {
            Ok(None)
        }
        async fn save(&self, _entity: &Entity) -> DomainResult<()> {
            Ok(())
        }
        async fn list_sanctioned(&self) -> DomainResult<Vec<Entity>> {
            Ok(Vec::new())
        }
    }

    fn service(
        transfers: MockTransfers,
        heuristics: HeuristicsConfig,
    ) -> RiskService<MockTransfers, NopEntities> {
        RiskService::new(
            transfers,
            NopEntities,
            RiskCacheConfig::default(),
            heuristics,
        )
    }

    #[tokio::test]
    async fn fan_out_triggers_at_threshold() {
        let repo = MockTransfers::default();
        let src = addr(1);
        let t0 = 1_000_000;
        for i in 0..5u32 {
            repo.push(make_transfer(&src, &addr(10 + i as u8), t0 + i as i64, i));
        }
        let svc = service(
            repo,
            HeuristicsConfig {
                min_fanout: 5,
                fan_window: Duration::from_secs(60),
                ..HeuristicsConfig::default()
            },
        );
        let ev = svc.detect_fan_out(&src).await.unwrap().expect("evidence");
        assert_eq!(ev.addresses().len(), 5);
        assert!(matches!(ev.heuristic(), ClusteringHeuristic::FanOut));
    }

    #[tokio::test]
    async fn fan_out_silent_below_threshold() {
        let repo = MockTransfers::default();
        let src = addr(1);
        for i in 0..4u32 {
            repo.push(make_transfer(
                &src,
                &addr(10 + i as u8),
                1_000_000 + i as i64,
                i,
            ));
        }
        let svc = service(
            repo,
            HeuristicsConfig {
                min_fanout: 5,
                fan_window: Duration::from_secs(60),
                ..HeuristicsConfig::default()
            },
        );
        assert!(svc.detect_fan_out(&src).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn fan_out_silent_when_spread_outside_window() {
        let repo = MockTransfers::default();
        let src = addr(1);
        // 5 distinct receivers, but spaced 1 day apart against a 60s window.
        for i in 0..5u32 {
            repo.push(make_transfer(
                &src,
                &addr(10 + i as u8),
                1_000_000 + (i as i64) * 86_400,
                i,
            ));
        }
        let svc = service(
            repo,
            HeuristicsConfig {
                min_fanout: 5,
                fan_window: Duration::from_secs(60),
                ..HeuristicsConfig::default()
            },
        );
        assert!(svc.detect_fan_out(&src).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn fan_in_triggers_at_threshold() {
        let repo = MockTransfers::default();
        let dst = addr(1);
        for i in 0..5u32 {
            repo.push(make_transfer(
                &addr(20 + i as u8),
                &dst,
                1_000_000 + i as i64,
                i,
            ));
        }
        let svc = service(
            repo,
            HeuristicsConfig {
                min_fanin: 5,
                fan_window: Duration::from_secs(60),
                ..HeuristicsConfig::default()
            },
        );
        let ev = svc.detect_fan_in(&dst).await.unwrap().expect("evidence");
        assert_eq!(ev.addresses().len(), 5);
        assert!(matches!(ev.heuristic(), ClusteringHeuristic::FanIn));
    }

    #[tokio::test]
    async fn smurfing_triggers_when_all_receivers_route_to_one_y() {
        let repo = MockTransfers::default();
        let distributor = addr(1);
        let cash_out = addr(99);
        for i in 0..5u32 {
            let r = addr(10 + i as u8);
            // distributor -> r at t0
            repo.push(make_transfer(&distributor, &r, 1_000_000 + i as i64, i));
            // r -> cash_out within window
            repo.push(make_transfer(&r, &cash_out, 1_001_000 + i as i64, 100 + i));
        }
        let svc = service(
            repo,
            HeuristicsConfig {
                min_fanout: 5,
                min_fanin: 5,
                fan_window: Duration::from_secs(60),
                smurf_window: Duration::from_secs(3_600),
                smurf_max_depth: 2,
                ..HeuristicsConfig::default()
            },
        );
        let ev = svc
            .detect_smurfing_cycle(&distributor)
            .await
            .unwrap()
            .expect("evidence");
        assert!(matches!(ev.heuristic(), ClusteringHeuristic::SmurfingCycle));
        assert_eq!(ev.addresses().first(), Some(&distributor));
        assert_eq!(ev.addresses().last(), Some(&cash_out));
        assert_eq!(ev.addresses().len(), 2 + 5);
    }

    #[tokio::test]
    async fn smurfing_silent_when_receivers_diverge() {
        let repo = MockTransfers::default();
        let distributor = addr(1);
        for i in 0..5u32 {
            let r = addr(10 + i as u8);
            let y = addr(90 + i as u8);
            repo.push(make_transfer(&distributor, &r, 1_000_000 + i as i64, i));
            repo.push(make_transfer(&r, &y, 1_001_000 + i as i64, 100 + i));
        }
        let svc = service(
            repo,
            HeuristicsConfig {
                min_fanout: 5,
                min_fanin: 5,
                fan_window: Duration::from_secs(60),
                smurf_window: Duration::from_secs(3_600),
                smurf_max_depth: 2,
                ..HeuristicsConfig::default()
            },
        );
        assert!(
            svc.detect_smurfing_cycle(&distributor)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn smurfing_finds_cash_out_two_hops_away() {
        let repo = MockTransfers::default();
        let distributor = addr(1);
        let cash_out = addr(99);
        for i in 0..5u32 {
            let r = addr(10 + i as u8);
            let m = addr(50 + i as u8);
            // distributor -> r -> m -> cash_out
            repo.push(make_transfer(&distributor, &r, 1_000_000 + i as i64, i));
            repo.push(make_transfer(&r, &m, 1_000_500 + i as i64, 100 + i));
            repo.push(make_transfer(&m, &cash_out, 1_001_000 + i as i64, 200 + i));
        }
        let svc = service(
            repo,
            HeuristicsConfig {
                min_fanout: 5,
                min_fanin: 5,
                fan_window: Duration::from_secs(60),
                smurf_window: Duration::from_secs(3_600),
                smurf_max_depth: 2,
                ..HeuristicsConfig::default()
            },
        );
        let ev = svc
            .detect_smurfing_cycle(&distributor)
            .await
            .unwrap()
            .expect("evidence");
        assert_eq!(ev.addresses().last(), Some(&cash_out));
    }

    /// Build a synthetic transfer with explicit (asset, decimals, raw_amount)
    /// — needed for the mixed-asset peeling regression below.
    fn make_transfer_with_asset(
        from: &Address,
        to: &Address,
        asset: AssetId,
        raw: u64,
        decimals: u8,
        ts_secs: i64,
        idx: u32,
    ) -> Transfer {
        let chain = ChainId::ETH;
        let mut hash = [0u8; 32];
        hash[28..32].copy_from_slice(&idx.to_be_bytes());
        Transfer::new(
            TransferId::new(chain, hash, idx),
            chain,
            TxRef::new(chain, hash),
            from.clone(),
            to.clone(),
            asset,
            Amount::new(U256::from(raw), decimals),
            BlockRef::new(chain, 100 + idx as u64, [0u8; 32]),
            Utc.timestamp_opt(ts_secs, 0).unwrap(),
            TransferKind::Native,
            Finality::Confirmed,
        )
    }

    /// Regression: before the fix, `detect_peeling_chain` summed amounts
    /// with `+` across heterogeneous assets — panics under
    /// `decimals mismatch left: 18 right: 6` whenever an address touched
    /// both ETH and USDC. The fix groups by asset and computes peeling per
    /// asset; this test passes mixed flows and asserts no panic.
    #[tokio::test]
    async fn peeling_chain_mixed_assets_does_not_panic() {
        let chain = ChainId::ETH;
        let usdc_addr = vec![0xa0u8; 20];
        let usdc = AssetId::contract(chain, usdc_addr);
        let eth = AssetId::native(chain);

        let middle = addr(1);
        let src = addr(2);
        let dst = addr(3);

        let repo = MockTransfers::default();
        // Native ETH inflow + outflow with different decimals (18).
        repo.push(make_transfer_with_asset(
            &src, &middle, eth.clone(), 1_000_000_000_000_000_000, 18, 1, 1,
        ));
        repo.push(make_transfer_with_asset(
            &middle, &dst, eth.clone(), 990_000_000_000_000_000, 18, 2, 2,
        ));
        // USDC inflow + outflow with decimals=6 in the SAME flow.
        repo.push(make_transfer_with_asset(
            &src,
            &middle,
            usdc.clone(),
            1_000_000_000,
            6,
            3,
            3,
        ));
        repo.push(make_transfer_with_asset(
            &middle,
            &dst,
            usdc.clone(),
            500_000_000,
            6,
            4,
            4,
        ));

        let svc = service(repo, HeuristicsConfig::default());
        // Before the fix this panicked at Amount::checked_add.
        let result = svc.detect_peeling_chain(&middle).await.unwrap();
        // ETH peels (≈ 1% retained) → evidence; USDC keeps 50% → ignored.
        let ev = result.expect("ETH leg should fire peeling");
        assert!(matches!(ev.heuristic(), ClusteringHeuristic::PeelingChain));
    }

    /// When ONLY a non-peeling asset is present (e.g. USDC: 50% retained),
    /// the detector must stay silent.
    #[tokio::test]
    async fn peeling_chain_silent_when_no_asset_peels() {
        let chain = ChainId::ETH;
        let usdc = AssetId::contract(chain, vec![0xa0u8; 20]);
        let middle = addr(1);
        let src = addr(2);
        let dst = addr(3);

        let repo = MockTransfers::default();
        repo.push(make_transfer_with_asset(
            &src, &middle, usdc.clone(), 1_000_000_000, 6, 1, 1,
        ));
        repo.push(make_transfer_with_asset(
            &middle, &dst, usdc.clone(), 500_000_000, 6, 2, 2,
        ));

        let svc = service(repo, HeuristicsConfig::default());
        assert!(svc.detect_peeling_chain(&middle).await.unwrap().is_none());
    }

    struct LabelledEntities {
        addr: Address,
        category: EntityCategory,
    }

    #[async_trait]
    impl EntityRepository for LabelledEntities {
        async fn find_by_id(&self, _id: &EntityId) -> DomainResult<Option<Entity>> {
            Ok(None)
        }
        async fn find_by_address(&self, addr: &Address) -> DomainResult<Option<Entity>> {
            if addr == &self.addr {
                let mut e = Entity::new(self.category.clone(), RiskScore::MEDIUM);
                e.add_address(addr.clone());
                Ok(Some(e))
            } else {
                Ok(None)
            }
        }
        async fn find_by_label(
            &self,
            _category: &EntityCategory,
            _label_name: &str,
        ) -> DomainResult<Option<Entity>> {
            Ok(None)
        }
        async fn save(&self, _entity: &Entity) -> DomainResult<()> {
            Ok(())
        }
        async fn list_sanctioned(&self) -> DomainResult<Vec<Entity>> {
            Ok(Vec::new())
        }
    }

    fn service_with_entities<E: EntityRepository>(
        transfers: MockTransfers,
        entities: E,
        heuristics: HeuristicsConfig,
    ) -> RiskService<MockTransfers, E> {
        RiskService::new(transfers, entities, RiskCacheConfig::default(), heuristics)
    }

    /// A burst that peaks above max_fanout must be rejected outright, not
    /// truncated to the cap — a truncated subset would still wrongly union
    /// a chunk of unrelated recipients together.
    #[tokio::test]
    async fn fan_out_silent_above_max_fanout() {
        let repo = MockTransfers::default();
        let src = addr(1);
        let t0 = 1_000_000;
        for i in 0..10u32 {
            repo.push(make_transfer(&src, &addr(10 + i as u8), t0 + i as i64, i));
        }
        let svc = service(
            repo,
            HeuristicsConfig {
                min_fanout: 5,
                max_fanout: 8,
                fan_window: Duration::from_secs(60),
                ..HeuristicsConfig::default()
            },
        );
        assert!(svc.detect_fan_out(&src).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn deposit_reuse_triggers_within_bounds() {
        let repo = MockTransfers::default();
        let deposit = addr(1);
        for i in 0..3u32 {
            repo.push(make_transfer(&addr(10 + i as u8), &deposit, 1_000_000 + i as i64, i));
        }
        let svc = service(repo, HeuristicsConfig::default());
        let ev = svc
            .deposit_reuse_cluster(&deposit)
            .await
            .unwrap()
            .expect("evidence");
        assert_eq!(ev.addresses().len(), 3);
        assert!(matches!(
            ev.heuristic(),
            ClusteringHeuristic::DepositAddressReuse
        ));
    }

    /// A deposit address reused by many more senders than max_fanin looks
    /// like a public hot wallet, not a personal deposit address — must stay
    /// silent even without any entity label.
    #[tokio::test]
    async fn deposit_reuse_silent_above_max_fanin() {
        let repo = MockTransfers::default();
        let deposit = addr(1);
        for i in 0..20u32 {
            repo.push(make_transfer(&addr(10 + i as u8), &deposit, 1_000_000 + i as i64, i));
        }
        let svc = service(
            repo,
            HeuristicsConfig {
                max_fanin: 10,
                ..HeuristicsConfig::default()
            },
        );
        assert!(svc.deposit_reuse_cluster(&deposit).await.unwrap().is_none());
    }

    /// An address already labelled as shared infrastructure (exchange,
    /// bridge, DeFi router, mixer, mining pool) must never be treated as a
    /// deposit-reuse merge point, regardless of sender count.
    #[tokio::test]
    async fn deposit_reuse_silent_when_labelled_exchange() {
        let repo = MockTransfers::default();
        let deposit = addr(1);
        for i in 0..3u32 {
            repo.push(make_transfer(&addr(10 + i as u8), &deposit, 1_000_000 + i as i64, i));
        }
        let svc = service_with_entities(
            repo,
            LabelledEntities {
                addr: deposit.clone(),
                category: EntityCategory::Exchange,
            },
            HeuristicsConfig::default(),
        );
        assert!(svc.deposit_reuse_cluster(&deposit).await.unwrap().is_none());
    }

    struct FixedKind {
        addr: Address,
        kind: AddressKind,
    }

    #[async_trait]
    impl AddressKindRepository for FixedKind {
        async fn kind(&self, addr: &Address) -> DomainResult<AddressKind> {
            if addr == &self.addr {
                Ok(self.kind.clone())
            } else {
                Ok(AddressKind::Unknown)
            }
        }
        async fn set_kind(&self, _addr: &Address, _kind: AddressKind) -> DomainResult<()> {
            Ok(())
        }
    }

    /// A deposit address already classified as a contract (DEX router,
    /// token, pool) must never fire deposit-reuse — contracts routinely
    /// receive from many unrelated senders by design, unlike a personal
    /// deposit address.
    #[tokio::test]
    async fn deposit_reuse_silent_when_address_is_a_contract() {
        let repo = MockTransfers::default();
        let deposit = addr(1);
        for i in 0..3u32 {
            repo.push(make_transfer(&addr(10 + i as u8), &deposit, 1_000_000 + i as i64, i));
        }
        let svc = service_with_entities(repo, NopEntities, HeuristicsConfig::default())
            .with_address_kinds(Arc::new(FixedKind {
                addr: deposit.clone(),
                kind: AddressKind::Contract,
            }));
        assert!(svc.deposit_reuse_cluster(&deposit).await.unwrap().is_none());
    }

    /// The lower thresholds (min incoming / min distinct senders) must come
    /// from `heuristics:` config, not be hardcoded — raising either bar
    /// above the fixture's actual counts must silence the detector.
    #[tokio::test]
    async fn deposit_reuse_respects_configured_min_senders() {
        let repo = MockTransfers::default();
        let deposit = addr(1);
        // 3 incoming transfers (meets default min_incoming) but only 2
        // distinct senders.
        for i in 0..3u32 {
            repo.push(make_transfer(
                &addr(10 + (i % 2) as u8),
                &deposit,
                1_000_000 + i as i64,
                i,
            ));
        }
        let svc = service(
            repo,
            HeuristicsConfig {
                deposit_reuse_min_senders: 3,
                ..HeuristicsConfig::default()
            },
        );
        assert!(svc.deposit_reuse_cluster(&deposit).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn deposit_reuse_respects_configured_min_incoming() {
        let repo = MockTransfers::default();
        let deposit = addr(1);
        for i in 0..3u32 {
            repo.push(make_transfer(&addr(10 + i as u8), &deposit, 1_000_000 + i as i64, i));
        }
        let svc = service(
            repo,
            HeuristicsConfig {
                deposit_reuse_min_incoming: 5,
                ..HeuristicsConfig::default()
            },
        );
        assert!(svc.deposit_reuse_cluster(&deposit).await.unwrap().is_none());
    }

    /// cluster_address must not union through a labelled hub even when the
    /// evidence surfacing it comes from a different heuristic (here: plain
    /// fan-in, which has no entity-awareness of its own) — the exclusion is
    /// applied generically at the merge step.
    #[tokio::test]
    async fn cluster_address_drops_labelled_hub_from_any_heuristic_evidence() {
        let repo = MockTransfers::default();
        let victim = addr(1);
        let hub = addr(99);
        let t0 = 1_000_000;
        // 4 ordinary receivers + the labelled hub as the 5th distinct
        // recipient — enough to trigger plain fan-out, which has no
        // entity-awareness of its own.
        for i in 0..4u32 {
            repo.push(make_transfer(&victim, &addr(10 + i as u8), t0 + i as i64, i));
        }
        repo.push(make_transfer(&victim, &hub, t0 + 4, 4));

        let svc = service_with_entities(
            repo,
            LabelledEntities {
                addr: hub.clone(),
                category: EntityCategory::Exchange,
            },
            HeuristicsConfig {
                min_fanout: 5,
                fan_window: Duration::from_secs(60),
                ..HeuristicsConfig::default()
            },
        );
        let components = svc.cluster_address(&victim).await.unwrap();

        assert!(
            components.iter().all(|c| !c.contains(&hub)),
            "labelled hub must not appear in any cluster component: {components:?}"
        );
        let victim_component = components
            .iter()
            .find(|c| c.contains(&victim))
            .expect("victim must be in some component");
        assert!(
            (0..4u32).all(|i| victim_component.contains(&addr(10 + i as u8))),
            "the 4 ordinary receivers must still be merged with victim: {victim_component:?}"
        );
    }

    /// Evidence must only carry counterparties from the peeling asset's own
    /// outgoing legs — a counterparty that received a different, non-peeling
    /// asset didn't participate in the pattern and must not leak in.
    #[tokio::test]
    async fn peeling_chain_evidence_excludes_non_peeling_asset_counterparties() {
        let chain = ChainId::ETH;
        let usdc = AssetId::contract(chain, vec![0xa0u8; 20]);
        let eth = AssetId::native(chain);

        let middle = addr(1);
        let src = addr(2);
        let eth_dst = addr(3);
        let usdc_dst = addr(4);

        let repo = MockTransfers::default();
        // ETH leg peels (~1% retained).
        repo.push(make_transfer_with_asset(
            &src, &middle, eth.clone(), 1_000_000_000_000_000_000, 18, 1, 1,
        ));
        repo.push(make_transfer_with_asset(
            &middle, &eth_dst, eth.clone(), 990_000_000_000_000_000, 18, 2, 2,
        ));
        // USDC leg does NOT peel (50% retained) and goes to a different
        // counterparty than the ETH leg.
        repo.push(make_transfer_with_asset(
            &src, &middle, usdc.clone(), 1_000_000_000, 6, 3, 3,
        ));
        repo.push(make_transfer_with_asset(
            &middle, &usdc_dst, usdc.clone(), 500_000_000, 6, 4, 4,
        ));

        let svc = service(repo, HeuristicsConfig::default());
        let ev = svc
            .detect_peeling_chain(&middle)
            .await
            .unwrap()
            .expect("ETH leg should fire peeling");
        assert!(ev.addresses().contains(&eth_dst));
        assert!(
            !ev.addresses().contains(&usdc_dst),
            "USDC counterparty must not leak into ETH-only peeling evidence: {:?}",
            ev.addresses()
        );
    }
}

#[cfg(test)]
mod score_config_tests {
    use super::*;
    use domain::chain::ChainId;
    use domain::primitives::Address;
    use domain::risk::{RiskEvidence, RiskSignal, RiskSignalKind};

    fn addr(seed: u8) -> Address {
        Address::new(ChainId::ETH, vec![seed; 20])
    }

    fn sig(severity: u8, sink_addr: Address) -> RiskSignal {
        use domain::trace::{Sink, SinkKind};
        // Sink::new derives risk_score from kind; for these tests only the
        // RiskSignal's severity drives aggregation, the sink is just a
        // distinguishable evidence-target for dedup.
        let sink = Sink::new(
            sink_addr,
            SinkKind::Mixer,
            domain::primitives::Amount::new(domain::primitives::U256::zero(), 18),
            domain::primitives::Ratio::ONE,
        );
        RiskSignal::new(
            RiskSignalKind::DirectExposure,
            RiskScore::new(severity),
            "test".into(),
            RiskEvidence::SinkExposure(vec![sink]),
        )
    }

    #[test]
    fn aggregate_empty_is_clean() {
        let cfg = ScoreConfig::default();
        assert_eq!(cfg.aggregate(&[]).value(), 0);
    }

    #[test]
    fn max_strategy_returns_max_severity() {
        let cfg = ScoreConfig {
            aggregation: ScoreAggregation::Max,
            ..ScoreConfig::default()
        };
        let signals = vec![
            sig(40, addr(1)),
            sig(75, addr(2)),
            sig(60, addr(3)),
        ];
        assert_eq!(cfg.aggregate(&signals).value(), 75);
    }

    #[test]
    fn weighted_count_stacks_extra_signals() {
        // max=75 + 0.5 * 60 + 0.5 * 40 = 75 + 30 + 20 = 125 → clamp to 100.
        let cfg = ScoreConfig {
            aggregation: ScoreAggregation::WeightedCount,
            count_bonus_weight: 0.5,
            ..ScoreConfig::default()
        };
        let signals = vec![
            sig(75, addr(1)),
            sig(60, addr(2)),
            sig(40, addr(3)),
        ];
        assert_eq!(cfg.aggregate(&signals).value(), 100);
    }

    #[test]
    fn weighted_count_below_cap_keeps_actual_value() {
        // max=50 + 0.2 * 40 + 0.2 * 30 = 50 + 8 + 6 = 64.
        let cfg = ScoreConfig {
            aggregation: ScoreAggregation::WeightedCount,
            count_bonus_weight: 0.2,
            max_score_cap: 100,
            ..ScoreConfig::default()
        };
        let signals = vec![
            sig(50, addr(1)),
            sig(40, addr(2)),
            sig(30, addr(3)),
        ];
        assert_eq!(cfg.aggregate(&signals).value(), 64);
    }

    #[test]
    fn dedup_collapses_repeated_sink_address() {
        // Same sink address appearing 3× should count as ONE signal.
        // Otherwise weighted_count would boost score by phantom evidence.
        let cfg = ScoreConfig {
            aggregation: ScoreAggregation::WeightedCount,
            count_bonus_weight: 0.5,
            ..ScoreConfig::default()
        };
        let same = addr(42);
        let signals = vec![
            sig(75, same.clone()),
            sig(75, same.clone()),
            sig(75, same.clone()),
        ];
        // All dedupe to one signal → no count bonus → score = max = 75.
        assert_eq!(cfg.aggregate(&signals).value(), 75);
        assert_eq!(cfg.unique_count(&signals), 1);
    }

    #[test]
    fn dedup_preserves_distinct_sinks() {
        // Same severity, different sink addresses → 3 unique signals,
        // weighted_count stacks the extras as expected.
        let cfg = ScoreConfig {
            aggregation: ScoreAggregation::WeightedCount,
            count_bonus_weight: 0.1,
            ..ScoreConfig::default()
        };
        let signals = vec![
            sig(75, addr(1)),
            sig(75, addr(2)),
            sig(75, addr(3)),
        ];
        // 75 + 0.1*75 + 0.1*75 = 75 + 7.5 + 7.5 = 90.
        assert_eq!(cfg.aggregate(&signals).value(), 90);
        assert_eq!(cfg.unique_count(&signals), 3);
    }

    #[test]
    fn max_cap_clamps() {
        let cfg = ScoreConfig {
            aggregation: ScoreAggregation::Max,
            max_score_cap: 50,
            ..ScoreConfig::default()
        };
        let signals = vec![sig(100, addr(1))];
        assert_eq!(cfg.aggregate(&signals).value(), 50);
    }
}
