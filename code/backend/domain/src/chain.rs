use crate::address_codec::AddressCodec;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChainId(u32);

impl ChainId {
    pub const ETH: Self = Self(1);
    pub const TRON: Self = Self(195);
    pub const BTC: Self = Self(0);
    pub const SOLANA: Self = Self(501);

    pub fn new(value: u32) -> Self {
        Self(value)
    }

    pub fn value(self) -> u32 {
        self.0
    }

    pub fn default_encoding(self) -> AddressEncoding {
        match self {
            Self::ETH => AddressEncoding::Hex20,
            Self::TRON => AddressEncoding::TronBase58Check,
            Self::BTC => AddressEncoding::Bech32,
            Self::SOLANA => AddressEncoding::Base58,
            _ => AddressEncoding::Hex20,
        }
    }
}

impl fmt::Display for ChainId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            ChainId::ETH => write!(f, "eth"),
            ChainId::TRON => write!(f, "tron"),
            ChainId::BTC => write!(f, "btc"),
            ChainId::SOLANA => write!(f, "sol"),
            ChainId(id) => write!(f, "chain:{id}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressModel {
    Account,
    Utxo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainFamily {
    Evm,
    Tron,
    Bitcoin,
    Solana,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressEncoding {
    Hex20,
    TronBase58Check,
    Bech32,
    Base58,
}

impl AddressEncoding {
    pub fn payload_len(self) -> usize {
        match self {
            Self::Hex20 => 20,
            Self::TronBase58Check => 21,
            Self::Bech32 => 20,
            Self::Base58 => 32,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChainMeta {
    id: ChainId,
    name: &'static str,
    family: ChainFamily,
    address_model: AddressModel,
    address_encoding: AddressEncoding,
    confirmation_depth: u64,
    native_asset_symbol: &'static str,
    native_asset_decimals: u8,
}

impl ChainMeta {
    pub fn new(
        id: ChainId,
        name: &'static str,
        family: ChainFamily,
        address_model: AddressModel,
        address_encoding: AddressEncoding,
        confirmation_depth: u64,
        native_asset_symbol: &'static str,
        native_asset_decimals: u8,
    ) -> Self {
        Self {
            id,
            name,
            family,
            address_model,
            address_encoding,
            confirmation_depth,
            native_asset_symbol,
            native_asset_decimals,
        }
    }

    pub fn id(&self) -> ChainId {
        self.id
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn family(&self) -> ChainFamily {
        self.family
    }

    pub fn address_model(&self) -> AddressModel {
        self.address_model
    }

    pub fn address_encoding(&self) -> AddressEncoding {
        self.address_encoding
    }

    pub fn confirmation_depth(&self) -> u64 {
        self.confirmation_depth
    }

    pub fn native_asset_symbol(&self) -> &'static str {
        self.native_asset_symbol
    }

    pub fn native_asset_decimals(&self) -> u8 {
        self.native_asset_decimals
    }

    pub fn codec(&self) -> &'static dyn AddressCodec {
        self.address_encoding.codec()
    }
}

#[derive(Clone)]
pub struct ChainRegistry {
    entries: Vec<ChainMeta>,
}

impl ChainRegistry {
    pub fn default_registry() -> Self {
        Self {
            entries: vec![
                ChainMeta::new(
                    ChainId::ETH,
                    "Ethereum",
                    ChainFamily::Evm,
                    AddressModel::Account,
                    AddressEncoding::Hex20,
                    12,
                    "ETH",
                    18,
                ),
                ChainMeta::new(
                    ChainId::TRON,
                    "Tron",
                    ChainFamily::Tron,
                    AddressModel::Account,
                    AddressEncoding::TronBase58Check,
                    20,
                    "TRX",
                    6,
                ),
                ChainMeta::new(
                    ChainId::BTC,
                    "Bitcoin",
                    ChainFamily::Bitcoin,
                    AddressModel::Utxo,
                    AddressEncoding::Bech32,
                    6,
                    "BTC",
                    8,
                ),
                ChainMeta::new(
                    ChainId::SOLANA,
                    "Solana",
                    ChainFamily::Solana,
                    AddressModel::Account,
                    AddressEncoding::Base58,
                    32,
                    "SOL",
                    9,
                ),
            ],
        }
    }

    pub fn get(&self, id: ChainId) -> Option<&ChainMeta> {
        self.entries.iter().find(|m| m.id() == id)
    }

    pub fn all(&self) -> &[ChainMeta] {
        &self.entries
    }

    pub fn register(&mut self, meta: ChainMeta) {
        self.entries.retain(|m| m.id() != meta.id());
        self.entries.push(meta);
    }

    pub fn encoding_for(&self, id: ChainId) -> AddressEncoding {
        self.get(id)
            .map(|m| m.address_encoding())
            .unwrap_or(AddressEncoding::Hex20)
    }
}

impl Default for ChainRegistry {
    fn default() -> Self {
        Self::default_registry()
    }
}
