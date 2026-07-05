use clap::Parser;
use serde::Deserialize;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(version, about)]
pub struct Cli {
    #[arg(short, long, default_value = "config.yaml")]
    config: String,
    #[arg(long)]
    port: Option<u16>,
    #[arg(long)]
    host: Option<String>,
    #[arg(long)]
    moralis_api_key: Option<String>,
    #[arg(long)]
    trongrid_api_key: Option<String>,
    #[arg(long)]
    etherscan_api_key: Option<String>,
    #[arg(long)]
    alchemy_api_key: Option<String>,
    #[arg(long)]
    socks_proxy: Option<String>,
    #[arg(long)]
    database_url: Option<String>,
    #[arg(long, help = "ETH chain source: moralis | bigquery | etherscan | routed")]
    eth_source: Option<String>,
    #[arg(long)]
    bigquery_project_id: Option<String>,
    #[arg(long)]
    bigquery_credentials_path: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct LogConfig {
    directives: Vec<String>,
}

impl LogConfig {
    pub fn directives(&self) -> &[String] {
        &self.directives
    }

    pub fn build_filter(&self) -> tracing_subscriber::EnvFilter {
        let directives = self.directives.join(",");
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&directives))
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct ServerConfig {
    host: String,
    port: u16,
    #[serde(default, rename = "api_key")]
    api_key: Option<String>,
    #[serde(default)]
    admin_api_key: Option<String>,
}

impl ServerConfig {
    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref().filter(|s| !s.is_empty())
    }

    pub fn admin_api_key(&self) -> Option<&str> {
        self.admin_api_key.as_deref().filter(|s| !s.is_empty())
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct MoralisConfig {
    base_url: String,
    /// Legacy single-key field. Merged into the resolved key list when set.
    #[serde(default)]
    api_key: Option<String>,
    /// Multi-key pool: each request picks round-robin; on 429 the offending
    /// key is cooled for `key_cooldown_secs` and the next non-cooled key
    /// is tried. When every key is cooled the source returns RateLimited
    /// so the router can fail over.
    #[serde(default)]
    api_keys: Vec<String>,
    #[serde(default = "MoralisConfig::default_key_cooldown_secs")]
    key_cooldown_secs: u64,
    /// Steady-state requests-per-second through the token bucket.
    #[serde(default = "MoralisConfig::default_requests_per_second")]
    requests_per_second: f64,
    #[serde(default = "MoralisConfig::default_requests_per_second_burst")]
    requests_per_second_burst: f64,
    /// HTTP retry budget for 429 / 5xx / network / truncated responses.
    #[serde(default = "MoralisConfig::default_http_max_attempts")]
    http_max_attempts: u8,
    #[serde(default)]
    cache: CacheConfigFile,
}

impl MoralisConfig {
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn default_key_cooldown_secs() -> u64 {
        5
    }
    fn default_requests_per_second() -> f64 {
        20.0
    }
    fn default_requests_per_second_burst() -> f64 {
        20.0
    }
    fn default_http_max_attempts() -> u8 {
        6
    }

    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    pub fn resolved_keys(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut push = |s: &str| {
            let k = s.trim();
            if !k.is_empty() && seen.insert(k.to_string()) {
                out.push(k.to_string());
            }
        };
        if let Some(k) = self.api_key.as_deref() {
            push(k);
        }
        for k in &self.api_keys {
            push(k);
        }
        out
    }

    pub fn has_any_key(&self) -> bool {
        !self.resolved_keys().is_empty()
    }

    pub fn cache(&self) -> &CacheConfigFile {
        &self.cache
    }

    /// Build the domain-level `MoralisEvmConfig` so main.rs doesn't need
    /// to thread every knob through the constructor.
    pub fn into_domain(self) -> infra::fetch_wallet_api::MoralisEvmConfig {
        let keys = self.resolved_keys();
        infra::fetch_wallet_api::MoralisEvmConfig::new(
            keys,
            self.base_url,
            self.cache.into_domain(),
            Some(self.requests_per_second),
            Some(self.requests_per_second_burst),
            Some(self.http_max_attempts),
            Some(Duration::from_secs(self.key_cooldown_secs)),
        )
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct TronGridConfigFile {
    #[serde(default = "TronGridConfigFile::default_base_url")]
    base_url: String,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default = "TronGridConfigFile::default_page_ttl_secs")]
    page_ttl_secs: u64,
    #[serde(default = "TronGridConfigFile::default_page_max_capacity")]
    page_max_capacity: u64,
    #[serde(default = "TronGridConfigFile::default_max_pages")]
    max_pages_per_endpoint: u32,
    #[serde(default)]
    file_cache: FileCacheConfig,
    #[serde(default = "TronGridConfigFile::default_enabled")]
    enabled: bool,
}

impl Default for TronGridConfigFile {
    fn default() -> Self {
        Self {
            base_url: Self::default_base_url(),
            api_key: None,
            page_ttl_secs: Self::default_page_ttl_secs(),
            page_max_capacity: Self::default_page_max_capacity(),
            max_pages_per_endpoint: Self::default_max_pages(),
            file_cache: FileCacheConfig::default(),
            enabled: Self::default_enabled(),
        }
    }
}

impl TronGridConfigFile {
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    fn default_base_url() -> String {
        "https://api.trongrid.io".into()
    }
    fn default_page_ttl_secs() -> u64 {
        3_600
    }
    fn default_page_max_capacity() -> u64 {
        10_000
    }
    fn default_max_pages() -> u32 {
        50
    }
    fn default_enabled() -> bool {
        true
    }

    pub fn into_domain(self) -> infra::TronGridConfig {
        infra::TronGridConfig::new(
            self.base_url,
            self.api_key,
            self.page_max_capacity,
            Duration::from_secs(self.page_ttl_secs),
            if self.file_cache.enabled {
                Some(std::path::PathBuf::from(self.file_cache.dir))
            } else {
                None
            },
            self.max_pages_per_endpoint,
        )
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct FileCacheConfig {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "FileCacheConfig::default_dir")]
    dir: String,
}

impl Default for FileCacheConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            dir: Self::default_dir(),
        }
    }
}

impl FileCacheConfig {
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn dir(&self) -> &str {
        &self.dir
    }

    fn default_dir() -> String {
        "./moralis_cache".into()
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct CacheConfigFile {
    #[serde(
        default = "CacheConfigFile::default_cold_ttl_secs",
        alias = "page_ttl_secs"
    )]
    cold_ttl_secs: u64,
    #[serde(default = "CacheConfigFile::default_hot_ttl_secs")]
    hot_ttl_secs: u64,
    #[serde(default = "CacheConfigFile::default_cache_hot_tail")]
    cache_hot_tail: bool,
    #[serde(default = "CacheConfigFile::default_confirmation_depth")]
    confirmation_depth: u64,
    #[serde(default = "CacheConfigFile::default_page_max_capacity")]
    page_max_capacity: u64,
    #[serde(default = "CacheConfigFile::default_latest_block_ttl_secs")]
    latest_block_ttl_secs: u64,
    #[serde(default)]
    file_cache: FileCacheConfig,
}

impl Default for CacheConfigFile {
    fn default() -> Self {
        Self {
            cold_ttl_secs: Self::default_cold_ttl_secs(),
            hot_ttl_secs: Self::default_hot_ttl_secs(),
            cache_hot_tail: Self::default_cache_hot_tail(),
            confirmation_depth: Self::default_confirmation_depth(),
            page_max_capacity: Self::default_page_max_capacity(),
            latest_block_ttl_secs: Self::default_latest_block_ttl_secs(),
            file_cache: FileCacheConfig::default(),
        }
    }
}

impl CacheConfigFile {
    fn default_cold_ttl_secs() -> u64 {
        86_400
    }
    fn default_hot_ttl_secs() -> u64 {
        15
    }
    fn default_cache_hot_tail() -> bool {
        true
    }
    fn default_confirmation_depth() -> u64 {
        12
    }
    fn default_page_max_capacity() -> u64 {
        100_000
    }
    fn default_latest_block_ttl_secs() -> u64 {
        15
    }

    pub fn into_domain(self) -> infra::fetch_wallet_api::CacheConfig {
        infra::fetch_wallet_api::CacheConfig::new(
            self.page_max_capacity,
            Duration::from_secs(self.cold_ttl_secs),
            Duration::from_secs(self.hot_ttl_secs),
            self.cache_hot_tail,
            self.confirmation_depth,
            Duration::from_secs(self.latest_block_ttl_secs),
            if self.file_cache.enabled {
                Some(std::path::PathBuf::from(self.file_cache.dir))
            } else {
                None
            },
        )
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct RiskCacheConfigFile {
    #[serde(default = "RiskCacheConfigFile::default_score_ttl_secs")]
    score_ttl_secs: u64,
    #[serde(default = "RiskCacheConfigFile::default_max_entries")]
    score_max_entries: u64,
    #[serde(default = "RiskCacheConfigFile::default_sanctions_ttl_secs")]
    sanctions_ttl_secs: u64,
    #[serde(default = "RiskCacheConfigFile::default_max_entries")]
    sanctions_max_entries: u64,
    #[serde(default = "RiskCacheConfigFile::default_trace_ttl_secs")]
    trace_ttl_secs: u64,
    #[serde(default = "RiskCacheConfigFile::default_trace_max_entries")]
    trace_max_entries: u64,
}

impl Default for RiskCacheConfigFile {
    fn default() -> Self {
        Self {
            score_ttl_secs: Self::default_score_ttl_secs(),
            score_max_entries: Self::default_max_entries(),
            sanctions_ttl_secs: Self::default_sanctions_ttl_secs(),
            sanctions_max_entries: Self::default_max_entries(),
            trace_ttl_secs: Self::default_trace_ttl_secs(),
            trace_max_entries: Self::default_trace_max_entries(),
        }
    }
}

impl RiskCacheConfigFile {
    fn default_score_ttl_secs() -> u64 {
        300
    }
    fn default_sanctions_ttl_secs() -> u64 {
        900
    }
    fn default_max_entries() -> u64 {
        10_000
    }
    fn default_trace_ttl_secs() -> u64 {
        300
    }
    fn default_trace_max_entries() -> u64 {
        2_000
    }

    pub fn into_domain(self) -> usecase::risk::RiskCacheConfig {
        usecase::risk::RiskCacheConfig {
            score_ttl: Duration::from_secs(self.score_ttl_secs),
            score_max_entries: self.score_max_entries,
            sanctions_ttl: Duration::from_secs(self.sanctions_ttl_secs),
            sanctions_max_entries: self.sanctions_max_entries,
            trace_ttl: Duration::from_secs(self.trace_ttl_secs),
            trace_max_entries: self.trace_max_entries,
        }
    }
}

/// Per-address risk score tunables: aggregation strategy + the trace
/// limits the score path walks. Everything here was previously hardcoded
/// in `RiskService::score()` — exposing as config so operators can tune
/// score sensitivity without rebuilding.
#[derive(Deserialize, Debug, Clone)]
pub struct ScoreConfigFile {
    #[serde(default = "ScoreConfigFile::default_aggregation")]
    pub aggregation: String,
    #[serde(default = "ScoreConfigFile::default_count_bonus_weight")]
    pub count_bonus_weight: f64,
    #[serde(default = "ScoreConfigFile::default_sink_severity_threshold")]
    pub sink_severity_threshold: u8,
    #[serde(default = "ScoreConfigFile::default_max_score_cap")]
    pub max_score_cap: u8,
    #[serde(default = "ScoreConfigFile::default_trace_max_depth")]
    pub trace_max_depth: u32,
    #[serde(default = "ScoreConfigFile::default_trace_max_nodes")]
    pub trace_max_nodes: usize,
    #[serde(default = "ScoreConfigFile::default_trace_max_paths")]
    pub trace_max_paths: usize,
    #[serde(default = "ScoreConfigFile::default_trace_min_amount_ratio_percent")]
    pub trace_min_amount_ratio_percent: u8,
}

impl Default for ScoreConfigFile {
    fn default() -> Self {
        Self {
            aggregation: Self::default_aggregation(),
            count_bonus_weight: Self::default_count_bonus_weight(),
            sink_severity_threshold: Self::default_sink_severity_threshold(),
            max_score_cap: Self::default_max_score_cap(),
            trace_max_depth: Self::default_trace_max_depth(),
            trace_max_nodes: Self::default_trace_max_nodes(),
            trace_max_paths: Self::default_trace_max_paths(),
            trace_min_amount_ratio_percent: Self::default_trace_min_amount_ratio_percent(),
        }
    }
}

impl ScoreConfigFile {
    fn default_aggregation() -> String {
        "weighted_count".into()
    }
    fn default_count_bonus_weight() -> f64 {
        0.1
    }
    fn default_sink_severity_threshold() -> u8 {
        75
    }
    fn default_max_score_cap() -> u8 {
        100
    }
    fn default_trace_max_depth() -> u32 {
        5
    }
    fn default_trace_max_nodes() -> usize {
        200
    }
    fn default_trace_max_paths() -> usize {
        100
    }
    fn default_trace_min_amount_ratio_percent() -> u8 {
        5
    }

    pub fn into_domain(self) -> usecase::risk::ScoreConfig {
        let aggregation = match self.aggregation.to_ascii_lowercase().as_str() {
            "max" => usecase::risk::ScoreAggregation::Max,
            "weighted_count" | "weighted-count" | "count" => {
                usecase::risk::ScoreAggregation::WeightedCount
            }
            other => {
                tracing::warn!(
                    aggregation = other,
                    "unknown score.aggregation, falling back to weighted_count"
                );
                usecase::risk::ScoreAggregation::WeightedCount
            }
        };
        usecase::risk::ScoreConfig {
            aggregation,
            count_bonus_weight: self.count_bonus_weight,
            sink_severity_threshold: self.sink_severity_threshold,
            max_score_cap: self.max_score_cap,
            trace_max_depth: self.trace_max_depth,
            trace_max_nodes: self.trace_max_nodes,
            trace_max_paths: self.trace_max_paths,
            trace_min_amount_ratio_percent: self.trace_min_amount_ratio_percent,
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct HeuristicsConfigFile {
    #[serde(default = "HeuristicsConfigFile::default_min_fanout")]
    min_fanout: usize,
    #[serde(default = "HeuristicsConfigFile::default_min_fanin")]
    min_fanin: usize,
    #[serde(default = "HeuristicsConfigFile::default_fan_window_secs")]
    fan_window_secs: u64,
    #[serde(default = "HeuristicsConfigFile::default_smurf_window_secs")]
    smurf_window_secs: u64,
    #[serde(default = "HeuristicsConfigFile::default_smurf_max_depth")]
    smurf_max_depth: u32,
    #[serde(default)]
    amount_tolerance: f64,
    #[serde(default = "HeuristicsConfigFile::default_burst_min_count")]
    burst_min_count: usize,
    #[serde(default = "HeuristicsConfigFile::default_burst_window_secs")]
    burst_window_secs: u64,
    #[serde(default = "HeuristicsConfigFile::default_burst_multiplier")]
    burst_multiplier: f64,
    #[serde(default = "HeuristicsConfigFile::default_fixed_amount_min_count")]
    fixed_amount_min_count: usize,
    #[serde(default = "HeuristicsConfigFile::default_fixed_amount_bucket_usd")]
    fixed_amount_bucket_usd: f64,
    #[serde(default = "HeuristicsConfigFile::default_dwell_max_secs")]
    dwell_max_secs: u64,
    #[serde(default = "HeuristicsConfigFile::default_dwell_min_pairs")]
    dwell_min_pairs: usize,
}

impl Default for HeuristicsConfigFile {
    fn default() -> Self {
        Self {
            min_fanout: Self::default_min_fanout(),
            min_fanin: Self::default_min_fanin(),
            fan_window_secs: Self::default_fan_window_secs(),
            smurf_window_secs: Self::default_smurf_window_secs(),
            smurf_max_depth: Self::default_smurf_max_depth(),
            amount_tolerance: 0.0,
            burst_min_count: Self::default_burst_min_count(),
            burst_window_secs: Self::default_burst_window_secs(),
            burst_multiplier: Self::default_burst_multiplier(),
            fixed_amount_min_count: Self::default_fixed_amount_min_count(),
            fixed_amount_bucket_usd: Self::default_fixed_amount_bucket_usd(),
            dwell_max_secs: Self::default_dwell_max_secs(),
            dwell_min_pairs: Self::default_dwell_min_pairs(),
        }
    }
}

impl HeuristicsConfigFile {
    fn default_min_fanout() -> usize {
        5
    }
    fn default_min_fanin() -> usize {
        5
    }
    fn default_fan_window_secs() -> u64 {
        86_400
    }
    fn default_smurf_window_secs() -> u64 {
        86_400
    }
    fn default_smurf_max_depth() -> u32 {
        2
    }
    fn default_burst_min_count() -> usize {
        20
    }
    fn default_burst_window_secs() -> u64 {
        3_600
    }
    fn default_burst_multiplier() -> f64 {
        5.0
    }
    fn default_fixed_amount_min_count() -> usize {
        5
    }
    fn default_fixed_amount_bucket_usd() -> f64 {
        100.0
    }
    fn default_dwell_max_secs() -> u64 {
        600
    }
    fn default_dwell_min_pairs() -> usize {
        5
    }

    pub fn into_domain(self) -> usecase::risk::HeuristicsConfig {
        usecase::risk::HeuristicsConfig {
            min_fanout: self.min_fanout,
            min_fanin: self.min_fanin,
            fan_window: Duration::from_secs(self.fan_window_secs),
            smurf_window: Duration::from_secs(self.smurf_window_secs),
            smurf_max_depth: self.smurf_max_depth,
            amount_tolerance: self.amount_tolerance,
            burst_min_count: self.burst_min_count,
            burst_window: Duration::from_secs(self.burst_window_secs),
            burst_multiplier: self.burst_multiplier,
            fixed_amount_min_count: self.fixed_amount_min_count,
            fixed_amount_bucket_usd: self.fixed_amount_bucket_usd,
            dwell_max_secs: self.dwell_max_secs,
            dwell_min_pairs: self.dwell_min_pairs,
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct DatabaseConfig {
    url: String,
}

impl DatabaseConfig {
    pub fn url(&self) -> &str {
        &self.url
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct ProxyConfig {
    socks_url: Option<String>,
}

impl ProxyConfig {
    pub fn socks_url(&self) -> Option<&str> {
        self.socks_url.as_deref()
    }
}

/// CORS knobs applied to the outer axum router. Each list accepts either
/// `["*"]` / `[]` (Any) or an explicit whitelist of exact values. Setting
/// `allow_credentials: true` together with a wildcard in any of the three
/// lists is a CORS spec violation and is rejected at startup.
#[derive(Deserialize, Debug, Clone)]
pub struct CorsConfig {
    #[serde(default = "CorsConfig::default_star")]
    allow_origins: Vec<String>,
    #[serde(default = "CorsConfig::default_star")]
    allow_methods: Vec<String>,
    #[serde(default = "CorsConfig::default_star")]
    allow_headers: Vec<String>,
    #[serde(default)]
    expose_headers: Vec<String>,
    #[serde(default)]
    allow_credentials: bool,
    #[serde(default)]
    max_age_secs: Option<u64>,
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            allow_origins: Self::default_star(),
            allow_methods: Self::default_star(),
            allow_headers: Self::default_star(),
            expose_headers: Vec::new(),
            allow_credentials: false,
            max_age_secs: None,
        }
    }
}

impl CorsConfig {
    fn default_star() -> Vec<String> {
        vec!["*".into()]
    }

    pub fn allow_origins(&self) -> &[String] {
        &self.allow_origins
    }

    pub fn allow_methods(&self) -> &[String] {
        &self.allow_methods
    }

    pub fn allow_headers(&self) -> &[String] {
        &self.allow_headers
    }

    pub fn expose_headers(&self) -> &[String] {
        &self.expose_headers
    }

    pub fn allow_credentials(&self) -> bool {
        self.allow_credentials
    }

    pub fn max_age(&self) -> Option<Duration> {
        self.max_age_secs.map(Duration::from_secs)
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct TelemetryConfig {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "TelemetryConfig::default_endpoint")]
    otlp_endpoint: String,
    #[serde(default = "TelemetryConfig::default_service_name")]
    service_name: String,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            otlp_endpoint: Self::default_endpoint(),
            service_name: Self::default_service_name(),
        }
    }
}

impl TelemetryConfig {
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn otlp_endpoint(&self) -> &str {
        &self.otlp_endpoint
    }

    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    fn default_endpoint() -> String {
        "http://localhost:4317".into()
    }
    fn default_service_name() -> String {
        "paytraces-api".into()
    }
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EthSourceKind {
    Moralis,
    Bigquery,
    Etherscan,
    /// Per-capability router that fans out across multiple inner sources.
    /// Configured under the `routed:` section; on `RateLimited` from one
    /// source it fails over to the next entry in the chain.
    Routed,
}

impl Default for EthSourceKind {
    fn default() -> Self {
        Self::Moralis
    }
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct EthereumConfig {
    #[serde(default)]
    source: EthSourceKind,
}

impl EthereumConfig {
    pub fn source(&self) -> EthSourceKind {
        self.source
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct BigQueryConfig {
    project_id: String,
    credentials_path: String,
    #[serde(default = "BigQueryConfig::default_transactions_table")]
    transactions_table: String,
    #[serde(default)]
    token_transfers_table: Option<String>,
    #[serde(default = "BigQueryConfig::default_max_rows")]
    max_rows_per_query: u64,
    #[serde(default = "BigQueryConfig::default_query_timeout_secs")]
    query_timeout_secs: u64,
}

impl BigQueryConfig {
    fn default_transactions_table() -> String {
        "bigquery-public-data.goog_blockchain_ethereum_mainnet_us.transactions".into()
    }
    fn default_max_rows() -> u64 {
        10_000
    }
    fn default_query_timeout_secs() -> u64 {
        60
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn credentials_path(&self) -> &str {
        &self.credentials_path
    }

    pub fn into_domain(self) -> infra::BigQueryEvmConfig {
        infra::BigQueryEvmConfig::new(
            self.project_id,
            std::path::PathBuf::from(self.credentials_path),
            self.transactions_table,
            self.token_transfers_table,
            self.max_rows_per_query,
            Duration::from_secs(self.query_timeout_secs),
        )
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct EtherscanConfigFile {
    /// Legacy single-key field. Kept for backward compat — when set, gets
    /// merged into the resolved key list. Prefer `api_keys`.
    #[serde(default)]
    api_key: Option<String>,
    /// Multi-key pool. Each request picks a key round-robin; on rate-limit
    /// the offending key is cooled for `key_cooldown_secs` and the next
    /// non-cooled key is tried. When every key is cooled the source
    /// returns `RateLimited` so a router upstream can fail over.
    #[serde(default)]
    api_keys: Vec<String>,
    #[serde(default = "EtherscanConfigFile::default_base_url")]
    base_url: String,
    #[serde(default)]
    page_size: Option<u32>,
    #[serde(default)]
    max_pages: Option<u32>,
    #[serde(default = "EtherscanConfigFile::default_cold_ttl_secs")]
    cold_ttl_secs: u64,
    #[serde(default = "EtherscanConfigFile::default_hot_ttl_secs")]
    hot_ttl_secs: u64,
    #[serde(default = "EtherscanConfigFile::default_cache_hot_tail")]
    cache_hot_tail: bool,
    #[serde(default = "EtherscanConfigFile::default_confirmation_depth")]
    confirmation_depth: u64,
    #[serde(default = "EtherscanConfigFile::default_latest_block_ttl_secs")]
    latest_block_ttl_secs: u64,
    #[serde(default = "EtherscanConfigFile::default_page_max_capacity")]
    page_max_capacity: u64,
    #[serde(default = "EtherscanConfigFile::default_max_concurrent_requests")]
    max_concurrent_requests: u32,
    #[serde(default = "EtherscanConfigFile::default_key_cooldown_secs")]
    key_cooldown_secs: u64,
    /// Steady-state requests per second. Free tier is 5 req/s; paid tier
    /// is higher. Drives the token-bucket throttle inside the source so
    /// we never burst above this rate regardless of concurrency.
    #[serde(default = "EtherscanConfigFile::default_requests_per_second")]
    requests_per_second: f64,
    /// Token-bucket burst capacity. Equal to `requests_per_second` by
    /// default — that's the conservative free-tier interpretation.
    #[serde(default = "EtherscanConfigFile::default_requests_per_second_burst")]
    requests_per_second_burst: f64,
    /// Retry budget on transient errors (429, 5xx, body-encoded rate-limit
    /// message, network failures) for `txlist`/`tokentx`/`txlistinternal`.
    #[serde(default = "EtherscanConfigFile::default_http_max_attempts")]
    http_max_attempts: u8,
    /// Same, but for `eth_getCode` (is_contract probe) — usually shorter
    /// since it's a best-effort probe.
    #[serde(default = "EtherscanConfigFile::default_is_contract_max_attempts")]
    is_contract_max_attempts: u8,
    #[serde(default)]
    file_cache: EtherscanFileCacheConfig,
}

impl EtherscanConfigFile {
    fn default_base_url() -> String {
        "https://api.etherscan.io/v2/api".into()
    }
    fn default_cold_ttl_secs() -> u64 {
        86_400
    }
    fn default_hot_ttl_secs() -> u64 {
        15
    }
    fn default_cache_hot_tail() -> bool {
        true
    }
    fn default_confirmation_depth() -> u64 {
        12
    }
    fn default_latest_block_ttl_secs() -> u64 {
        15
    }
    fn default_page_max_capacity() -> u64 {
        10_000
    }
    fn default_max_concurrent_requests() -> u32 {
        // Etherscan free tier caps at 5 req/sec; leave headroom so bursts of
        // tokio::try_join! native+token calls across BFS nodes do not trip the
        // throttle. The HTTP layer also retries on rate-limit responses.
        4
    }

    fn default_key_cooldown_secs() -> u64 {
        5
    }

    fn default_requests_per_second() -> f64 {
        5.0
    }

    fn default_requests_per_second_burst() -> f64 {
        5.0
    }

    fn default_http_max_attempts() -> u8 {
        6
    }

    fn default_is_contract_max_attempts() -> u8 {
        4
    }

    /// Merged key list: legacy `api_key` (if any) plus `api_keys`, dedupe
    /// preserving order. Empty / whitespace entries are dropped.
    pub fn resolved_keys(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let push = |s: &str, out: &mut Vec<String>, seen: &mut std::collections::HashSet<String>| {
            let k = s.trim();
            if k.is_empty() || !seen.insert(k.to_string()) {
                return;
            }
            out.push(k.to_string());
        };
        if let Some(k) = self.api_key.as_deref() {
            push(k, &mut out, &mut seen);
        }
        for k in &self.api_keys {
            push(k, &mut out, &mut seen);
        }
        out
    }

    pub fn has_any_key(&self) -> bool {
        !self.resolved_keys().is_empty()
    }

    pub fn into_domain(self) -> infra::EtherscanEvmConfig {
        let keys = self.resolved_keys();
        infra::EtherscanEvmConfig::new(
            keys,
            Some(self.base_url),
            self.page_size,
            self.max_pages,
            Duration::from_secs(self.cold_ttl_secs),
            Duration::from_secs(self.hot_ttl_secs),
            self.cache_hot_tail,
            self.confirmation_depth,
            Duration::from_secs(self.latest_block_ttl_secs),
            self.page_max_capacity,
            if self.file_cache.enabled {
                Some(std::path::PathBuf::from(self.file_cache.dir))
            } else {
                None
            },
            self.max_concurrent_requests,
            Some(Duration::from_secs(self.key_cooldown_secs)),
            Some(self.requests_per_second),
            Some(self.requests_per_second_burst),
            Some(self.http_max_attempts),
            Some(self.is_contract_max_attempts),
        )
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct AlchemyConfigFile {
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    api_keys: Vec<String>,
    #[serde(default = "AlchemyConfigFile::default_base_url")]
    base_url: String,
    #[serde(default = "AlchemyConfigFile::default_key_cooldown_secs")]
    key_cooldown_secs: u64,
    #[serde(default = "AlchemyConfigFile::default_latest_block_ttl_secs")]
    latest_block_ttl_secs: u64,
    #[serde(default = "AlchemyConfigFile::default_max_concurrent_requests")]
    max_concurrent_requests: u32,
    /// When `false` (default) the Alchemy source rejects
    /// `transfers_for_address` with `InsufficientData("alchemy: transfers
    /// disabled")`. The router then propagates the error rather than
    /// silently degrading — leave this off if you only want Alchemy as an
    /// `is_contract` / `latest_block` fallback.
    #[serde(default)]
    enable_transfers: bool,
    #[serde(default = "AlchemyConfigFile::default_log_chunk_blocks")]
    log_chunk_blocks: u64,
    #[serde(default = "AlchemyConfigFile::default_min_log_chunk_blocks")]
    min_log_chunk_blocks: u64,
    #[serde(default = "AlchemyConfigFile::default_trace_page_size")]
    trace_page_size: u32,
    #[serde(default = "AlchemyConfigFile::default_trace_max_pages")]
    trace_max_pages: u32,
    #[serde(default = "AlchemyConfigFile::default_cold_ttl_secs")]
    cold_ttl_secs: u64,
    #[serde(default = "AlchemyConfigFile::default_hot_ttl_secs")]
    hot_ttl_secs: u64,
    #[serde(default = "AlchemyConfigFile::default_cache_hot_tail")]
    cache_hot_tail: bool,
    #[serde(default = "AlchemyConfigFile::default_confirmation_depth")]
    confirmation_depth: u64,
    #[serde(default = "AlchemyConfigFile::default_page_max_capacity")]
    page_max_capacity: u64,
    /// Steady-state requests per second. Alchemy free tier is ~25-30 RPS
    /// for compute-units based throttling. Tune down if you share the key.
    #[serde(default = "AlchemyConfigFile::default_requests_per_second")]
    requests_per_second: f64,
    #[serde(default = "AlchemyConfigFile::default_requests_per_second_burst")]
    requests_per_second_burst: f64,
    /// Retry budget on transient errors (HTTP 429, RPC error code 429 /
    /// -32007, 5xx, network failures).
    #[serde(default = "AlchemyConfigFile::default_http_max_attempts")]
    http_max_attempts: u8,
}

impl AlchemyConfigFile {
    fn default_base_url() -> String {
        infra::alchemy_evm_source::DEFAULT_BASE_URL.into()
    }
    fn default_key_cooldown_secs() -> u64 {
        5
    }
    fn default_latest_block_ttl_secs() -> u64 {
        15
    }
    fn default_max_concurrent_requests() -> u32 {
        8
    }
    fn default_log_chunk_blocks() -> u64 {
        2_000
    }
    fn default_min_log_chunk_blocks() -> u64 {
        16
    }
    fn default_trace_page_size() -> u32 {
        1_000
    }
    fn default_trace_max_pages() -> u32 {
        50
    }
    fn default_cold_ttl_secs() -> u64 {
        86_400
    }
    fn default_hot_ttl_secs() -> u64 {
        15
    }
    fn default_cache_hot_tail() -> bool {
        true
    }
    fn default_confirmation_depth() -> u64 {
        12
    }
    fn default_page_max_capacity() -> u64 {
        100_000
    }
    fn default_requests_per_second() -> f64 {
        // Free-tier Alchemy: 500 CU/sec sustained. The rate limiter is
        // CU-priced for this source; each call pays its method's CU.
        500.0
    }
    fn default_requests_per_second_burst() -> f64 {
        // Alchemy's 10s burst window: up to 5000 CU spent in any 10s.
        5_000.0
    }
    fn default_http_max_attempts() -> u8 {
        5
    }

    pub fn resolved_keys(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut push = |s: &str| {
            let k = s.trim();
            if !k.is_empty() && seen.insert(k.to_string()) {
                out.push(k.to_string());
            }
        };
        if let Some(k) = self.api_key.as_deref() {
            push(k);
        }
        for k in &self.api_keys {
            push(k);
        }
        out
    }

    pub fn has_any_key(&self) -> bool {
        !self.resolved_keys().is_empty()
    }

    pub fn into_domain(self) -> infra::AlchemyEvmConfig {
        let keys = self.resolved_keys();
        infra::AlchemyEvmConfig::new(
            keys,
            Some(self.base_url),
            Some(Duration::from_secs(self.key_cooldown_secs)),
            Some(Duration::from_secs(self.latest_block_ttl_secs)),
            Some(self.max_concurrent_requests),
            self.enable_transfers,
            Some(self.log_chunk_blocks),
            Some(self.min_log_chunk_blocks),
            Some(self.trace_page_size),
            Some(self.trace_max_pages),
            Some(Duration::from_secs(self.cold_ttl_secs)),
            Some(Duration::from_secs(self.hot_ttl_secs)),
            Some(self.cache_hot_tail),
            Some(self.confirmation_depth),
            Some(self.page_max_capacity),
            Some(self.requests_per_second),
            Some(self.requests_per_second_burst),
            Some(self.http_max_attempts),
        )
    }
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct RoutedConfigFile {
    #[serde(default)]
    pub transfers: Vec<String>,
    #[serde(default)]
    pub is_contract: Vec<String>,
    #[serde(default)]
    pub latest_block: Vec<String>,
    #[serde(default)]
    pub fetch_block: Vec<String>,
    #[serde(default = "RoutedConfigFile::default_source_cooldown_secs")]
    pub source_cooldown_secs: u64,
}

impl RoutedConfigFile {
    fn default_source_cooldown_secs() -> u64 {
        10
    }

    pub fn source_cooldown(&self) -> Duration {
        Duration::from_secs(self.source_cooldown_secs)
    }

    /// Union of all source names referenced across the four capability
    /// chains — main.rs uses this to know which leaf sources need to be
    /// constructed.
    pub fn referenced_sources(&self) -> std::collections::HashSet<String> {
        let mut out = std::collections::HashSet::new();
        for v in [
            &self.transfers,
            &self.is_contract,
            &self.latest_block,
            &self.fetch_block,
        ] {
            for n in v {
                out.insert(n.clone());
            }
        }
        out
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct EtherscanFileCacheConfig {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "EtherscanFileCacheConfig::default_dir")]
    dir: String,
}

impl Default for EtherscanFileCacheConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            dir: Self::default_dir(),
        }
    }
}

impl EtherscanFileCacheConfig {
    fn default_dir() -> String {
        "./etherscan_cache".into()
    }
}

/// Per-chain source spec inside the `chains: [...]` list. Each chain declares
/// which upstream (or router) serves it; provider-specific pools of API keys
/// and throttles stay in the top-level `alchemy:`/`etherscan:`/`moralis:`/
/// `trongrid:` sections. Overrides (`base_url`, table names) are folded on top
/// of those defaults at wire-up time.
#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ChainSourceSpec {
    Etherscan {
        #[serde(default)]
        base_url: Option<String>,
    },
    Alchemy {
        /// Chain-specific Alchemy endpoint. When absent, uses `alchemy.base_url`
        /// from the top-level section (which is ETH-mainnet by default).
        /// For non-ETH deployments you MUST override — Alchemy's chain-specific
        /// hosts (polygon-mainnet.g.alchemy.com, base-mainnet.g.alchemy.com, ...)
        /// share the pool of API keys but differ in URL.
        #[serde(default)]
        base_url: Option<String>,
    },
    Moralis,
    Bigquery {
        #[serde(default)]
        transactions_table: Option<String>,
        #[serde(default)]
        token_transfers_table: Option<String>,
    },
    Trongrid {
        #[serde(default)]
        base_url: Option<String>,
    },
    Routed {
        #[serde(default)]
        transfers: Vec<String>,
        #[serde(default)]
        is_contract: Vec<String>,
        #[serde(default)]
        latest_block: Vec<String>,
        #[serde(default)]
        fetch_block: Vec<String>,
        #[serde(default)]
        source_cooldown_secs: Option<u64>,
    },
}

impl ChainSourceSpec {
    /// Names of leaf providers this spec references. For `routed`, that's the
    /// union of every capability chain; for a single-source spec, it's the
    /// one provider it names. Used by main.rs to build only what's needed.
    pub fn referenced_providers(&self) -> std::collections::HashSet<String> {
        let mut out = std::collections::HashSet::new();
        match self {
            ChainSourceSpec::Etherscan { .. } => {
                out.insert("etherscan".to_string());
            }
            ChainSourceSpec::Alchemy { .. } => {
                out.insert("alchemy".to_string());
            }
            ChainSourceSpec::Moralis => {
                out.insert("moralis".to_string());
            }
            ChainSourceSpec::Bigquery { .. } => {
                out.insert("bigquery".to_string());
            }
            ChainSourceSpec::Trongrid { .. } => {
                out.insert("trongrid".to_string());
            }
            ChainSourceSpec::Routed {
                transfers,
                is_contract,
                latest_block,
                fetch_block,
                ..
            } => {
                for v in [transfers, is_contract, latest_block, fetch_block] {
                    for n in v {
                        out.insert(n.clone());
                    }
                }
            }
        }
        out
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct ChainConfigFile {
    /// Numeric chain id — must match `domain::chain::ChainId` values (1 = ETH
    /// mainnet, 137 = Polygon, 56 = BSC, 195 = Tron, etc). Duplicate ids are
    /// rejected at startup.
    pub id: u32,
    /// Optional human-readable name; if omitted, `ChainRegistry::default_registry`
    /// value (or `"chain:<id>"`) is used in logs. This is purely cosmetic.
    #[serde(default)]
    pub name: Option<String>,
    /// Which upstream (or router) serves this chain.
    pub source: ChainSourceSpec,
}

#[derive(Deserialize, Debug, Clone)]
pub struct AppConfig {
    server: ServerConfig,
    moralis: MoralisConfig,
    #[serde(default)]
    trongrid: TronGridConfigFile,
    #[serde(default)]
    bigquery: Option<BigQueryConfig>,
    #[serde(default)]
    etherscan: Option<EtherscanConfigFile>,
    #[serde(default)]
    alchemy: Option<AlchemyConfigFile>,
    #[serde(default)]
    routed: Option<RoutedConfigFile>,
    #[serde(default)]
    ethereum: EthereumConfig,
    /// New: per-chain wire-up. When non-empty, replaces the legacy
    /// `ethereum:` / `trongrid.enabled:` path entirely.
    #[serde(default)]
    chains: Vec<ChainConfigFile>,
    database: DatabaseConfig,
    proxy: ProxyConfig,
    log: LogConfig,
    #[serde(default)]
    telemetry: TelemetryConfig,
    #[serde(default)]
    cors: CorsConfig,
    #[serde(default)]
    risk_cache: RiskCacheConfigFile,
    #[serde(default)]
    heuristics: HeuristicsConfigFile,
    #[serde(default)]
    score: ScoreConfigFile,
    #[serde(default)]
    labels: LabelsConfigFile,
    #[serde(default)]
    ingestion: IngestionConfigFile,
    #[serde(default)]
    api: ApiConfig,
}

/// Tunables for adaptive concurrency in the ingestion pipeline. The gate
/// throttles concurrent `transfers_for_address` calls across parallel
/// /build_graph requests. On `RateLimited` from the chain source, the gate
/// permanently forgets the held permit (shrinking effective capacity by 1)
/// down to `min`; on `grow_after_successes` consecutive Oks it grows back
/// up to `max`. Both shifts log at WARN.
#[derive(Deserialize, Debug, Clone)]
pub struct IngestionConfigFile {
    #[serde(default)]
    pub transfers_concurrency: TransfersConcurrencyConfig,
    /// Batch size for `classify_address_kinds` — addresses per
    /// `ChainSource::is_contract_batch` call. With Alchemy this turns into
    /// one JSON-RPC batched HTTP per chunk; with Etherscan into N parallel
    /// `eth_getCode` calls. 100 is comfortable for both upstreams.
    #[serde(default = "IngestionConfigFile::default_classify_chain_batch_size")]
    pub classify_chain_batch_size: usize,
}

impl IngestionConfigFile {
    fn default_classify_chain_batch_size() -> usize {
        100
    }
}

impl Default for IngestionConfigFile {
    fn default() -> Self {
        Self {
            transfers_concurrency: TransfersConcurrencyConfig::default(),
            classify_chain_batch_size: Self::default_classify_chain_batch_size(),
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct TransfersConcurrencyConfig {
    #[serde(default = "TransfersConcurrencyConfig::default_initial")]
    pub initial: u32,
    #[serde(default = "TransfersConcurrencyConfig::default_min")]
    pub min: u32,
    #[serde(default = "TransfersConcurrencyConfig::default_max")]
    pub max: u32,
    #[serde(default = "TransfersConcurrencyConfig::default_grow_after_successes")]
    pub grow_after_successes: u32,
}

impl Default for TransfersConcurrencyConfig {
    fn default() -> Self {
        Self {
            initial: Self::default_initial(),
            min: Self::default_min(),
            max: Self::default_max(),
            grow_after_successes: Self::default_grow_after_successes(),
        }
    }
}

impl TransfersConcurrencyConfig {
    fn default_initial() -> u32 {
        4
    }
    fn default_min() -> u32 {
        1
    }
    fn default_max() -> u32 {
        8
    }
    fn default_grow_after_successes() -> u32 {
        20
    }
}

/// Cross-cutting API knobs that don't fit into more specific sections.
///
/// `default_chain_id` — the chain used when a request omits `chain_id`. Historically
/// hard-coded to 1 (ETH); making it a config value is the first step to running
/// this server against a non-Ethereum primary chain (Polygon, BSC, ...). Requests
/// that still want ETH just pass `chain_id=1` explicitly.
#[derive(Deserialize, Debug, Clone)]
pub struct ApiConfig {
    #[serde(default = "ApiConfig::default_chain_id_default")]
    default_chain_id: u32,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            default_chain_id: Self::default_chain_id_default(),
        }
    }
}

impl ApiConfig {
    fn default_chain_id_default() -> u32 {
        1
    }

    pub fn default_chain_id(&self) -> u32 {
        self.default_chain_id
    }
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct LabelsConfigFile {
    #[serde(default)]
    bootstrap_file: Option<String>,
}

impl LabelsConfigFile {
    pub fn bootstrap_file(&self) -> Option<&str> {
        self.bootstrap_file.as_deref().filter(|s| !s.is_empty())
    }
}

impl AppConfig {
    pub fn server(&self) -> &ServerConfig {
        &self.server
    }

    pub fn moralis(&self) -> &MoralisConfig {
        &self.moralis
    }

    pub fn trongrid(&self) -> &TronGridConfigFile {
        &self.trongrid
    }

    pub fn database(&self) -> &DatabaseConfig {
        &self.database
    }

    pub fn proxy(&self) -> &ProxyConfig {
        &self.proxy
    }

    pub fn log(&self) -> &LogConfig {
        &self.log
    }

    pub fn telemetry(&self) -> &TelemetryConfig {
        &self.telemetry
    }

    pub fn cors(&self) -> &CorsConfig {
        &self.cors
    }

    pub fn risk_cache(&self) -> &RiskCacheConfigFile {
        &self.risk_cache
    }

    pub fn heuristics(&self) -> &HeuristicsConfigFile {
        &self.heuristics
    }

    pub fn score(&self) -> &ScoreConfigFile {
        &self.score
    }

    pub fn labels(&self) -> &LabelsConfigFile {
        &self.labels
    }

    pub fn ingestion(&self) -> &IngestionConfigFile {
        &self.ingestion
    }

    pub fn api(&self) -> &ApiConfig {
        &self.api
    }

    pub fn chains(&self) -> &[ChainConfigFile] {
        &self.chains
    }

    pub fn ethereum(&self) -> &EthereumConfig {
        &self.ethereum
    }

    pub fn bigquery(&self) -> Option<&BigQueryConfig> {
        self.bigquery.as_ref()
    }

    pub fn etherscan(&self) -> Option<&EtherscanConfigFile> {
        self.etherscan.as_ref()
    }

    pub fn alchemy(&self) -> Option<&AlchemyConfigFile> {
        self.alchemy.as_ref()
    }

    pub fn routed(&self) -> Option<&RoutedConfigFile> {
        self.routed.as_ref()
    }

    pub fn load(cli: &Cli) -> anyhow::Result<Self> {
        let mut builder = config::Config::builder()
            .add_source(config::File::with_name(&cli.config).required(false))
            .add_source(
                config::Environment::default()
                    .separator("__")
                    .try_parsing(true),
            );

        let overrides: &[(&str, &dyn Fn(&Cli) -> Option<String>)] = &[
            ("server.port", &|c| c.port.map(|p| p.to_string())),
            ("server.host", &|c| c.host.clone()),
            ("moralis.api_key", &|c| c.moralis_api_key.clone()),
            ("trongrid.api_key", &|c| c.trongrid_api_key.clone()),
            ("etherscan.api_key", &|c| c.etherscan_api_key.clone()),
            ("alchemy.api_key", &|c| c.alchemy_api_key.clone()),
            ("proxy.socks_url", &|c| c.socks_proxy.clone()),
            ("database.url", &|c| c.database_url.clone()),
            ("ethereum.source", &|c| c.eth_source.clone()),
            ("bigquery.project_id", &|c| c.bigquery_project_id.clone()),
            ("bigquery.credentials_path", &|c| {
                c.bigquery_credentials_path.clone()
            }),
        ];

        for (key, extract) in overrides {
            if let Some(value) = extract(cli) {
                builder = builder.set_override(key, value)?;
            }
        }

        Ok(builder.build()?.try_deserialize()?)
    }
}
