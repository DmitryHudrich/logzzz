use std::{path::PathBuf, sync::Arc, time::Duration};

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode as jwt_encode};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use domain::{
    asset::{AssetId, TokenStandard},
    chain::ChainId,
    error::{DomainError, DomainResult},
    ports::{BlockRange, ChainSource},
    primitives::{Address, Amount, BlockRef, TxRef, U256},
    transfer::{Finality, NormalizedBlock, Transfer, TransferId, TransferKind},
};

const SCOPE: &str = "https://www.googleapis.com/auth/bigquery.readonly";
const BQ_API_BASE: &str = "https://bigquery.googleapis.com/bigquery/v2";
const ACCESS_TOKEN_SKEW_SECS: i64 = 30;
const ERC20_DEFAULT_DECIMALS: u8 = 18;

#[derive(Debug, Clone)]
pub struct BigQueryEvmConfig {
    project_id: String,
    credentials_path: PathBuf,
    transactions_table: String,
    token_transfers_table: Option<String>,
    max_rows_per_query: u64,
    query_timeout: Duration,
}

impl BigQueryEvmConfig {
    pub fn new(
        project_id: String,
        credentials_path: PathBuf,
        transactions_table: String,
        token_transfers_table: Option<String>,
        max_rows_per_query: u64,
        query_timeout: Duration,
    ) -> Self {
        Self {
            project_id,
            credentials_path,
            transactions_table,
            token_transfers_table,
            max_rows_per_query,
            query_timeout,
        }
    }

    /// Override the transactions dataset table. Different chains live in
    /// different BQ public datasets — `bigquery-public-data.crypto_ethereum.*`
    /// for ETH, `crypto_polygon.*` for Polygon, etc. The default is ETH.
    pub fn with_transactions_table(mut self, table: String) -> Self {
        self.transactions_table = table;
        self
    }

    /// Override (or unset) the ERC-20 transfers table. `None` disables
    /// token-transfer harvesting entirely for this chain.
    pub fn with_token_transfers_table(mut self, table: Option<String>) -> Self {
        self.token_transfers_table = table;
        self
    }
}

#[derive(Debug, Deserialize)]
struct ServiceAccountKey {
    client_email: String,
    private_key: String,
    #[serde(default)]
    private_key_id: Option<String>,
    token_uri: String,
}

#[derive(Debug, Clone)]
struct CachedToken {
    access_token: String,
    expires_at_unix: i64,
}

#[derive(Clone)]
pub struct BigQueryEvmSource {
    chain: ChainId,
    project_id: String,
    transactions_table: String,
    token_transfers_table: Option<String>,
    max_rows_per_query: u64,
    query_timeout: Duration,
    client: reqwest::Client,
    service_account: Arc<ServiceAccountKey>,
    cached_token: Arc<Mutex<Option<CachedToken>>>,
}

impl BigQueryEvmSource {
    pub async fn new(
        chain: ChainId,
        client: reqwest::Client,
        cfg: BigQueryEvmConfig,
    ) -> DomainResult<Self> {
        let raw = tokio::fs::read_to_string(&cfg.credentials_path)
            .await
            .map_err(|e| {
                DomainError::InsufficientData(format!(
                    "BigQuery: cannot read service account at {}: {e}",
                    cfg.credentials_path.display()
                ))
            })?;
        let sa: ServiceAccountKey = serde_json::from_str(&raw).map_err(|e| {
            DomainError::InsufficientData(format!(
                "BigQuery: invalid service account JSON at {}: {e}",
                cfg.credentials_path.display()
            ))
        })?;

        tracing::info!(
            project = %cfg.project_id,
            transactions_table = %cfg.transactions_table,
            token_transfers_table = ?cfg.token_transfers_table,
            client_email = %sa.client_email,
            chain_id = chain.value(),
            "BigQuery EVM source initialized"
        );

        Ok(Self {
            chain,
            project_id: cfg.project_id,
            transactions_table: cfg.transactions_table,
            token_transfers_table: cfg.token_transfers_table,
            max_rows_per_query: cfg.max_rows_per_query.max(1),
            query_timeout: cfg.query_timeout,
            client,
            service_account: Arc::new(sa),
            cached_token: Arc::new(Mutex::new(None)),
        })
    }

    async fn access_token(&self) -> DomainResult<String> {
        let now = Utc::now().timestamp();
        {
            let guard = self.cached_token.lock().await;
            if let Some(t) = guard.as_ref()
                && t.expires_at_unix - ACCESS_TOKEN_SKEW_SECS > now
            {
                return Ok(t.access_token.clone());
            }
        }

        tracing::debug!("BigQuery: refreshing OAuth2 access token");
        let sa = &self.service_account;

        let claims = JwtClaims {
            iss: sa.client_email.clone(),
            scope: SCOPE.to_string(),
            aud: sa.token_uri.clone(),
            iat: now,
            exp: now + 3600,
        };

        let mut header = Header::new(Algorithm::RS256);
        header.kid = sa.private_key_id.clone();
        let key = EncodingKey::from_rsa_pem(sa.private_key.as_bytes()).map_err(|e| {
            DomainError::InsufficientData(format!("BigQuery: invalid private key: {e}"))
        })?;
        let jwt = jwt_encode(&header, &claims, &key).map_err(|e| {
            DomainError::InsufficientData(format!("BigQuery: JWT sign failed: {e}"))
        })?;

        // JWT chars are URL-safe base64 ([A-Za-z0-9_-]) so no percent-encoding needed;
        // the grant_type literal is percent-encoded explicitly.
        let body = format!(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion={jwt}"
        );

        let resp = self
            .client
            .post(&sa.token_uri)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body)
            .send()
            .await
            .map_err(|e| {
                DomainError::InsufficientData(format!("BigQuery: token exchange HTTP failed: {e}"))
            })?;

        let status = resp.status();
        let body = resp.text().await.map_err(|e| {
            DomainError::InsufficientData(format!("BigQuery: read token body failed: {e}"))
        })?;
        if !status.is_success() {
            tracing::error!(status = status.as_u16(), body = %body, "BigQuery token error");
            return Err(DomainError::InsufficientData(format!(
                "BigQuery token endpoint returned {status}: {body}"
            )));
        }

        #[derive(Deserialize)]
        struct TokenResp {
            access_token: String,
            expires_in: i64,
        }
        let tr: TokenResp = serde_json::from_str(&body).map_err(|e| {
            DomainError::InsufficientData(format!("BigQuery: parse token response: {e}: {body}"))
        })?;

        let token = CachedToken {
            access_token: tr.access_token.clone(),
            expires_at_unix: now + tr.expires_in,
        };
        let mut guard = self.cached_token.lock().await;
        *guard = Some(token);

        tracing::debug!(expires_in = tr.expires_in, "BigQuery: access token acquired");
        Ok(tr.access_token)
    }

    async fn run_query(
        &self,
        sql: &str,
        params: Vec<QueryParameter>,
    ) -> DomainResult<QueryResponse> {
        let token = self.access_token().await?;
        let url = format!("{}/projects/{}/queries", BQ_API_BASE, self.project_id);

        let req_body = QueryRequest {
            query: sql.to_string(),
            use_legacy_sql: false,
            parameter_mode: "NAMED".into(),
            query_parameters: params,
            timeout_ms: self.query_timeout.as_millis().min(u32::MAX as u128) as u32,
            max_results: self.max_rows_per_query,
        };

        tracing::debug!(
            project = %self.project_id,
            timeout_ms = req_body.timeout_ms,
            max_results = req_body.max_results,
            "BigQuery: submitting jobs.query"
        );

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&token)
            .json(&req_body)
            .send()
            .await
            .map_err(|e| {
                DomainError::InsufficientData(format!("BigQuery: jobs.query HTTP failed: {e}"))
            })?;

        let status = resp.status();
        let body = resp.text().await.map_err(|e| {
            DomainError::InsufficientData(format!("BigQuery: read jobs.query body failed: {e}"))
        })?;
        if !status.is_success() {
            tracing::error!(
                status = status.as_u16(),
                body_preview = %body.chars().take(500).collect::<String>(),
                "BigQuery jobs.query error"
            );
            return Err(DomainError::InsufficientData(format!(
                "BigQuery jobs.query returned {status}: {body}"
            )));
        }

        let parsed: QueryResponse = serde_json::from_str(&body).map_err(|e| {
            DomainError::InsufficientData(format!(
                "BigQuery: parse jobs.query response: {e}; body preview: {}",
                body.chars().take(500).collect::<String>()
            ))
        })?;

        if !parsed.job_complete {
            return Err(DomainError::InsufficientData(format!(
                "BigQuery: job did not complete within timeout {} ms",
                req_body.timeout_ms
            )));
        }

        tracing::debug!(
            rows = parsed.rows.as_ref().map(Vec::len).unwrap_or(0),
            total_bytes_processed = parsed.total_bytes_processed.as_deref(),
            cache_hit = parsed.cache_hit.unwrap_or(false),
            "BigQuery: query done"
        );

        Ok(parsed)
    }

    async fn fetch_native(
        &self,
        address_hex_lower: &str,
        from_block: Option<u64>,
        to_block: Option<u64>,
    ) -> DomainResult<Vec<Transfer>> {
        let from = from_block.unwrap_or(0);
        let to = to_block.unwrap_or(i64::MAX as u64);

        // Schema of `goog_blockchain_ethereum_mainnet_us.transactions`:
        //   transaction_hash STRING, from_address STRING, to_address STRING(nullable),
        //   value BIGNUMERIC, value_lossless STRING (full 256-bit wei),
        //   block_number INT64, block_hash STRING, block_timestamp TIMESTAMP.
        // We use value_lossless to avoid BIGNUMERIC truncation on edge cases.
        // receipt_status is not in this table — rows are treated as Confirmed.
        let sql = format!(
            "SELECT transaction_hash, from_address, to_address, value_lossless, \
             block_number, block_hash, block_timestamp \
             FROM `{table}` \
             WHERE (from_address = @addr OR to_address = @addr) \
               AND value_lossless != '0' \
               AND block_number BETWEEN @from_block AND @to_block \
             ORDER BY block_number DESC \
             LIMIT @max_rows",
            table = self.transactions_table,
        );

        let params = vec![
            QueryParameter::string("addr", address_hex_lower),
            QueryParameter::int64("from_block", from as i64),
            QueryParameter::int64("to_block", to as i64),
            QueryParameter::int64("max_rows", self.max_rows_per_query as i64),
        ];

        let resp = self.run_query(&sql, params).await?;
        let mut out = Vec::new();
        if let Some(rows) = resp.rows {
            for (idx, row) in rows.into_iter().enumerate() {
                match map_native_row(self.chain, &row) {
                    Ok(Some(t)) => out.push(t),
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!(idx, error = %e, "BigQuery: skip malformed native row");
                    }
                }
            }
        }
        Ok(out)
    }

    async fn fetch_erc20(
        &self,
        address_hex_lower: &str,
        from_block: Option<u64>,
        to_block: Option<u64>,
    ) -> DomainResult<Vec<Transfer>> {
        let Some(table) = self.token_transfers_table.as_deref() else {
            tracing::debug!("BigQuery: token_transfers_table not configured — skipping ERC-20");
            return Ok(Vec::new());
        };

        let from = from_block.unwrap_or(0);
        let to = to_block.unwrap_or(i64::MAX as u64);

        // Schema of `goog_blockchain_ethereum_mainnet_us.token_transfers`:
        //   address STRING (token contract — event emitter, NULLABLE),
        //   from_address STRING, to_address STRING, quantity STRING,
        //   transaction_hash STRING, event_index INT64,
        //   block_number INT64, block_hash STRING, block_timestamp TIMESTAMP,
        //   event_type STRING ('ERC20' / 'ERC721' / 'ERC1155'),
        //   removed BOOLEAN (orphaned events).
        let sql = format!(
            "SELECT address, from_address, to_address, quantity, \
             transaction_hash, event_index, block_number, block_hash, block_timestamp \
             FROM `{table}` \
             WHERE (from_address = @addr OR to_address = @addr) \
               AND event_type = 'ERC-20' \
               AND (removed IS NULL OR removed = FALSE) \
               AND address IS NOT NULL \
               AND block_number BETWEEN @from_block AND @to_block \
             ORDER BY block_number DESC \
             LIMIT @max_rows",
        );

        let params = vec![
            QueryParameter::string("addr", address_hex_lower),
            QueryParameter::int64("from_block", from as i64),
            QueryParameter::int64("to_block", to as i64),
            QueryParameter::int64("max_rows", self.max_rows_per_query as i64),
        ];

        let resp = self.run_query(&sql, params).await?;
        let mut out = Vec::new();
        if let Some(rows) = resp.rows {
            for (idx, row) in rows.into_iter().enumerate() {
                match map_erc20_row(self.chain, &row) {
                    Ok(t) => out.push(t),
                    Err(e) => {
                        tracing::warn!(idx, error = %e, "BigQuery: skip malformed erc20 row");
                    }
                }
            }
        }
        Ok(out)
    }
}

#[async_trait]
impl ChainSource for BigQueryEvmSource {
    fn chain_id(&self) -> ChainId {
        self.chain
    }

    async fn latest_block(&self) -> DomainResult<BlockRef> {
        let sql = format!(
            "SELECT MAX(block_number) AS h FROM `{table}`",
            table = self.transactions_table,
        );
        let resp = self.run_query(&sql, Vec::new()).await?;
        let height = resp
            .rows
            .as_ref()
            .and_then(|rs| rs.first())
            .and_then(|r| field_str(r, 0))
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| {
                DomainError::InsufficientData(
                    "BigQuery: latest_block returned no rows / null max(block_number)".into(),
                )
            })?;
        tracing::debug!(height, "BigQuery: latest_block resolved");
        Ok(BlockRef::new(self.chain, height, [0u8; 32]))
    }

    async fn fetch_block(&self, height: u64) -> DomainResult<NormalizedBlock> {
        Err(DomainError::InsufficientData(format!(
            "BigQuery: fetch_block by height ({height}) not supported; use transfers_for_address"
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
                "BigQuery source (chain={}) called with foreign address chain: {}",
                self.chain,
                addr.chain()
            )));
        }

        let address_hex = format!("0x{}", hex::encode(addr.bytes()));
        let from_block = (range.from_height() > 0).then(|| range.from_height());
        let to_block = (range.to_height() < u64::MAX).then(|| range.to_height());

        tracing::info!(
            address = %address_hex,
            from_block,
            to_block,
            max_transfers,
            "BigQuery: fetching ETH transfers"
        );

        let (native, erc20) = tokio::try_join!(
            self.fetch_native(&address_hex, from_block, to_block),
            self.fetch_erc20(&address_hex, from_block, to_block),
        )?;

        tracing::info!(
            address = %address_hex,
            native = native.len(),
            erc20 = erc20.len(),
            total = native.len() + erc20.len(),
            "BigQuery: transfers fetched"
        );

        let mut all = native;
        all.extend(erc20);
        if all.len() > max_transfers {
            all.truncate(max_transfers);
        }
        Ok(all)
    }
}

// ── JWT / request DTOs ───────────────────────────────────────────────────────

#[derive(Serialize)]
struct JwtClaims {
    iss: String,
    scope: String,
    aud: String,
    iat: i64,
    exp: i64,
}

#[derive(Serialize)]
struct QueryRequest {
    query: String,
    #[serde(rename = "useLegacySql")]
    use_legacy_sql: bool,
    #[serde(rename = "parameterMode")]
    parameter_mode: String,
    #[serde(rename = "queryParameters")]
    query_parameters: Vec<QueryParameter>,
    #[serde(rename = "timeoutMs")]
    timeout_ms: u32,
    #[serde(rename = "maxResults")]
    max_results: u64,
}

#[derive(Serialize)]
struct QueryParameter {
    name: String,
    #[serde(rename = "parameterType")]
    parameter_type: ParameterType,
    #[serde(rename = "parameterValue")]
    parameter_value: ParameterValue,
}

#[derive(Serialize)]
struct ParameterType {
    #[serde(rename = "type")]
    ty: &'static str,
}

#[derive(Serialize)]
struct ParameterValue {
    value: String,
}

impl QueryParameter {
    fn string(name: &str, value: &str) -> Self {
        Self {
            name: name.into(),
            parameter_type: ParameterType { ty: "STRING" },
            parameter_value: ParameterValue {
                value: value.to_string(),
            },
        }
    }

    fn int64(name: &str, value: i64) -> Self {
        Self {
            name: name.into(),
            parameter_type: ParameterType { ty: "INT64" },
            parameter_value: ParameterValue {
                value: value.to_string(),
            },
        }
    }
}

#[derive(Deserialize, Debug)]
struct QueryResponse {
    #[serde(rename = "jobComplete", default)]
    job_complete: bool,
    #[serde(default)]
    rows: Option<Vec<Row>>,
    #[serde(rename = "totalBytesProcessed", default)]
    total_bytes_processed: Option<String>,
    #[serde(rename = "cacheHit", default)]
    cache_hit: Option<bool>,
}

#[derive(Deserialize, Debug)]
struct Row {
    #[serde(default)]
    f: Vec<Cell>,
}

#[derive(Deserialize, Debug)]
struct Cell {
    #[serde(default)]
    v: Option<serde_json::Value>,
}

fn field_str(row: &Row, idx: usize) -> Option<&str> {
    row.f.get(idx).and_then(|c| c.v.as_ref()).and_then(|v| v.as_str())
}

// ── Row mappers ──────────────────────────────────────────────────────────────

fn map_native_row(chain: ChainId, row: &Row) -> anyhow::Result<Option<Transfer>> {
    use anyhow::{Context, anyhow};

    let hash = field_str(row, 0).ok_or_else(|| anyhow!("native: missing transaction_hash"))?;
    let from_s = field_str(row, 1).ok_or_else(|| anyhow!("native: missing from_address"))?;
    let to_s = field_str(row, 2);
    let value_s = field_str(row, 3).ok_or_else(|| anyhow!("native: missing value"))?;
    let block_num_s = field_str(row, 4).ok_or_else(|| anyhow!("native: missing block_number"))?;
    let block_hash_s = field_str(row, 5);
    let ts_s = field_str(row, 6).ok_or_else(|| anyhow!("native: missing block_timestamp"))?;

    let Some(to_s) = to_s.filter(|s| !s.is_empty()) else {
        return Ok(None);
    };

    let tx_hash = parse_hash32(hash).context("native: tx hash")?;
    let block_number: u64 = block_num_s.parse().context("native: block_number")?;
    let block_hash = block_hash_s
        .map(parse_hash32)
        .transpose()
        .context("native: block_hash")?
        .unwrap_or(tx_hash);
    let timestamp = parse_bq_timestamp(ts_s).context("native: block_timestamp")?;
    let raw = parse_u256_from_bq(value_s).context("native: value")?;
    if raw.is_zero() {
        return Ok(None);
    }
    let from = parse_eth_address(chain, from_s).context("native: from_address")?;
    let to = parse_eth_address(chain, to_s).context("native: to_address")?;

    Ok(Some(Transfer::new(
        TransferId::new(chain, tx_hash, 0),
        chain,
        TxRef::new(chain, tx_hash),
        from,
        to,
        AssetId::native(chain),
        Amount::new(raw, 18),
        BlockRef::new(chain, block_number, block_hash),
        timestamp,
        TransferKind::Native,
        Finality::Confirmed,
    )))
}

fn map_erc20_row(chain: ChainId, row: &Row) -> anyhow::Result<Transfer> {
    use anyhow::{Context, anyhow};

    let token_addr_s = field_str(row, 0).ok_or_else(|| anyhow!("erc20: missing token_address"))?;
    let from_s = field_str(row, 1).ok_or_else(|| anyhow!("erc20: missing from_address"))?;
    let to_s = field_str(row, 2).ok_or_else(|| anyhow!("erc20: missing to_address"))?;
    let value_s = field_str(row, 3).ok_or_else(|| anyhow!("erc20: missing value"))?;
    let tx_hash_s = field_str(row, 4).ok_or_else(|| anyhow!("erc20: missing transaction_hash"))?;
    let event_index_s = field_str(row, 5);
    let block_num_s = field_str(row, 6).ok_or_else(|| anyhow!("erc20: missing block_number"))?;
    let block_hash_s = field_str(row, 7);
    let ts_s = field_str(row, 8).ok_or_else(|| anyhow!("erc20: missing block_timestamp"))?;

    let tx_hash = parse_hash32(tx_hash_s).context("erc20: tx hash")?;
    let block_number: u64 = block_num_s.parse().context("erc20: block_number")?;
    let block_hash = block_hash_s
        .map(parse_hash32)
        .transpose()
        .context("erc20: block_hash")?
        .unwrap_or(tx_hash);
    let timestamp = parse_bq_timestamp(ts_s).context("erc20: block_timestamp")?;
    let event_index: u32 = event_index_s
        .map(|s| s.parse::<u32>())
        .transpose()
        .context("erc20: event_index")?
        .unwrap_or(0);
    // idx=0 is reserved for the (single) native transfer in this tx; shift
    // token rows by +1 so they never collide on the (chain, tx_hash, idx) PK
    // when a tx has both a native value transfer and an ERC-20 Transfer event
    // (e.g. WETH deposit) — otherwise ON CONFLICT DO UPDATE silently overwrites
    // one with the other and the token is lost.
    let idx = event_index.saturating_add(1);
    let raw = parse_u256_from_bq(value_s).context("erc20: value")?;

    let from = parse_eth_address(chain, from_s).context("erc20: from_address")?;
    let to = parse_eth_address(chain, to_s).context("erc20: to_address")?;
    let contract = parse_eth_address(chain, token_addr_s).context("erc20: token_address")?;

    Ok(Transfer::new(
        TransferId::new(chain, tx_hash, idx),
        chain,
        TxRef::new(chain, tx_hash),
        from,
        to,
        AssetId::contract(chain, contract.bytes().to_vec()),
        Amount::new(raw, ERC20_DEFAULT_DECIMALS),
        BlockRef::new(chain, block_number, block_hash),
        timestamp,
        TransferKind::Token {
            contract,
            standard: TokenStandard::Erc20,
            symbol: None,
        },
        Finality::Confirmed,
    ))
}

fn parse_hash32(s: &str) -> anyhow::Result<[u8; 32]> {
    use anyhow::{anyhow, Context};
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

fn parse_u256_from_bq(s: &str) -> anyhow::Result<U256> {
    use anyhow::anyhow;
    // BigQuery NUMERIC is serialized like "1234.000000000"; strip trailing
    // fractional part (always all-zero for whole-wei values from the chain).
    let int_part = match s.split_once('.') {
        Some((int, frac)) => {
            if !frac.chars().all(|c| c == '0') {
                return Err(anyhow!("unexpected non-zero fractional part: {s}"));
            }
            int
        }
        None => s,
    };
    U256::from_dec_str(int_part.trim()).map_err(|e| anyhow!("U256 parse '{s}': {e}"))
}

fn parse_bq_timestamp(s: &str) -> anyhow::Result<DateTime<Utc>> {
    use anyhow::anyhow;
    // BigQuery REST API serializes TIMESTAMP as a string of seconds (with
    // fractional microseconds) since the Unix epoch, e.g. "1612137600.000000".
    let f: f64 = s.parse().map_err(|e| anyhow!("bad bq timestamp '{s}': {e}"))?;
    let secs = f.trunc() as i64;
    let nanos = ((f - secs as f64) * 1_000_000_000.0).round() as u32;
    chrono::Utc
        .timestamp_opt(secs, nanos)
        .single()
        .ok_or_else(|| anyhow!("ambiguous/out-of-range bq timestamp '{s}'"))
}
