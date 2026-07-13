use serde::Deserialize;

#[derive(Deserialize, Debug, Default)]
pub struct TransactionListResponse {
    #[serde(default)]
    data: Vec<RawTransaction>,
}

impl TransactionListResponse {
    pub fn into_data(self) -> Vec<RawTransaction> {
        self.data
    }
}

#[derive(Deserialize, Debug)]
pub struct RawTransaction {
    hash: String,
    timestamp: i64,
    #[serde(rename = "ownerAddress")]
    owner_address: String,
    #[serde(rename = "toAddress", default)]
    to_address: Option<String>,
    #[serde(rename = "contractType")]
    contract_type: i64,
    #[serde(rename = "contractRet", default)]
    contract_ret: Option<String>,
    #[serde(default)]
    amount: Option<String>,
}

/// Tron's `TransferContract` type id — the only native-TRX contract kind
/// that represents a value transfer (see `wallet/broadcasttransaction`
/// protobuf `Transaction.Contract.ContractType`).
pub const TRANSFER_CONTRACT_TYPE: i64 = 1;

impl RawTransaction {
    pub fn hash(&self) -> &str {
        &self.hash
    }

    pub fn timestamp(&self) -> i64 {
        self.timestamp
    }

    pub fn owner_address(&self) -> &str {
        &self.owner_address
    }

    pub fn to_address(&self) -> Option<&str> {
        self.to_address.as_deref()
    }

    pub fn contract_type(&self) -> i64 {
        self.contract_type
    }

    pub fn contract_ret(&self) -> Option<&str> {
        self.contract_ret.as_deref()
    }

    pub fn amount(&self) -> Option<&str> {
        self.amount.as_deref()
    }
}

#[derive(Deserialize, Debug, Default)]
pub struct Trc20TransferListResponse {
    #[serde(default, rename = "token_transfers")]
    token_transfers: Vec<Trc20Transfer>,
}

impl Trc20TransferListResponse {
    pub fn into_transfers(self) -> Vec<Trc20Transfer> {
        self.token_transfers
    }
}

#[derive(Deserialize, Debug)]
pub struct Trc20Transfer {
    transaction_id: String,
    block_ts: i64,
    from_address: String,
    to_address: String,
    contract_address: String,
    quant: String,
    #[serde(default, rename = "tokenInfo")]
    token_info: Trc20TokenInfo,
}

impl Trc20Transfer {
    pub fn transaction_id(&self) -> &str {
        &self.transaction_id
    }

    pub fn block_ts(&self) -> i64 {
        self.block_ts
    }

    pub fn from_address(&self) -> &str {
        &self.from_address
    }

    pub fn to_address(&self) -> &str {
        &self.to_address
    }

    pub fn contract_address(&self) -> &str {
        &self.contract_address
    }

    pub fn quant(&self) -> &str {
        &self.quant
    }

    pub fn token_info(&self) -> &Trc20TokenInfo {
        &self.token_info
    }
}

/// Tronscan only populates `tokenInfo` for tokens it has indexed metadata
/// for (its "vip"/whitelisted set, which covers USDT, USDC, WTRX and other
/// tokens that matter for tracing). For obscure/unlisted contracts this
/// comes back as `{}` — every field absent — and there is no other Tronscan
/// endpoint that reliably resolves decimals by contract address, so callers
/// must treat a `None` `token_decimal` as "cannot normalize this transfer".
#[derive(Deserialize, Debug, Default)]
pub struct Trc20TokenInfo {
    #[serde(default, rename = "tokenAbbr")]
    token_abbr: Option<String>,
    #[serde(default, rename = "tokenDecimal")]
    token_decimal: Option<u8>,
}

impl Trc20TokenInfo {
    pub fn token_abbr(&self) -> Option<&str> {
        self.token_abbr.as_deref()
    }

    pub fn token_decimal(&self) -> Option<u8> {
        self.token_decimal
    }
}

/// `/api/account` response, trimmed to the fields this source needs:
/// `accountType == 2` marks a smart contract, and `addressTag`/
/// `addressTagLogo` carry Tronscan's curated public label for the address
/// (e.g. "Binance-Cold 2"), when it has one.
#[derive(Deserialize, Debug, Default)]
pub struct AccountInfo {
    #[serde(default, rename = "accountType")]
    account_type: i64,
    #[serde(default, rename = "addressTag")]
    address_tag: Option<String>,
    #[serde(default, rename = "addressTagLogo")]
    address_tag_logo: Option<String>,
}

impl AccountInfo {
    pub fn is_contract(&self) -> bool {
        self.account_type == 2
    }

    /// Curated public tag name, or `None` if Tronscan has no label for this
    /// address (absent from the response, or present but blank).
    pub fn address_tag(&self) -> Option<&str> {
        self.address_tag.as_deref().filter(|s| !s.is_empty())
    }

    pub fn address_tag_logo(&self) -> Option<&str> {
        self.address_tag_logo.as_deref().filter(|s| !s.is_empty())
    }
}

#[derive(Deserialize, Debug)]
pub struct LatestBlock {
    number: u64,
    hash: String,
    timestamp: i64,
}

impl LatestBlock {
    pub fn number(&self) -> u64 {
        self.number
    }

    pub fn hash(&self) -> &str {
        &self.hash
    }

    pub fn timestamp(&self) -> i64 {
        self.timestamp
    }
}
