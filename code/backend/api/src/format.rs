use crate::error::ApiError;
use domain::chain::{ChainId, ChainRegistry};
use domain::entity::SanctionList;
use domain::primitives::{Address, U256};
use domain::risk::RiskSignalKind;
use domain::trace::SinkKind;
use domain::transfer::TransferKind;

pub fn parse_address(s: &str, chain: ChainId) -> Result<Address, ApiError> {
    Address::parse(chain, s).map_err(|e| ApiError::bad_request(e.to_string()))
}

pub fn format_amount(raw: U256, decimals: u8) -> String {
    let s = raw.to_string();
    if decimals == 0 {
        return s;
    }
    let dec = decimals as usize;
    let (int_part, frac_part) = if s.len() <= dec {
        (String::from("0"), format!("{:0>w$}", s, w = dec))
    } else {
        let split = s.len() - dec;
        (s[..split].to_string(), s[split..].to_string())
    };
    let frac_trunc = if frac_part.len() > 8 {
        &frac_part[..8]
    } else {
        &frac_part[..]
    };
    let frac_trim = frac_trunc.trim_end_matches('0');
    if frac_trim.is_empty() {
        int_part
    } else {
        format!("{int_part}.{frac_trim}")
    }
}

pub fn native_symbol(chains: &ChainRegistry, chain: ChainId) -> String {
    chains
        .get(chain)
        .map(|m| m.native_asset_symbol().to_string())
        .unwrap_or_default()
}

pub fn transfer_kind_str(k: &TransferKind) -> (&'static str, Option<String>) {
    match k {
        TransferKind::Native => ("native", None),
        TransferKind::Token { contract, .. } => ("token", Some(contract.canonical())),
        TransferKind::Internal => ("internal", None),
        TransferKind::Fee => ("fee", None),
        TransferKind::UtxoEdge { .. } => ("utxo_edge", None),
    }
}

pub fn edge_symbol(k: &TransferKind, native: &str) -> String {
    match k {
        TransferKind::Native | TransferKind::Internal | TransferKind::Fee => native.to_string(),
        TransferKind::Token { symbol, .. } => symbol.clone().unwrap_or_default(),
        TransferKind::UtxoEdge { .. } => native.to_string(),
    }
}

pub fn sink_kind_str(k: &SinkKind) -> (&'static str, Option<String>) {
    match k {
        SinkKind::Exchange { name, .. } => ("exchange", Some(name.clone())),
        SinkKind::Bridge { .. } => ("bridge", None),
        SinkKind::Mixer => ("mixer", None),
        SinkKind::Sanctioned => ("sanctioned", None),
        SinkKind::Darknet => ("darknet", None),
        SinkKind::Unresolved => ("unresolved", None),
    }
}

pub fn signal_kind_str(k: &RiskSignalKind) -> &'static str {
    match k {
        RiskSignalKind::DirectExposure => "direct_exposure",
        RiskSignalKind::IndirectExposure { .. } => "indirect_exposure",
        RiskSignalKind::SanctionedCounterparty => "sanctioned_counterparty",
        RiskSignalKind::MixerInteraction => "mixer_interaction",
        RiskSignalKind::DarknetMarket => "darknet_market",
        RiskSignalKind::RapidLayering => "rapid_layering",
        RiskSignalKind::HighVelocity => "high_velocity",
        RiskSignalKind::NewAddress => "new_address",
        RiskSignalKind::NoKyc => "no_kyc",
    }
}

pub fn sanction_list_str(s: &SanctionList) -> String {
    match s {
        SanctionList::Ofac => "ofac".into(),
        SanctionList::Eu => "eu".into(),
        SanctionList::Un => "un".into(),
        SanctionList::Other(s) => s.to_lowercase().replace([' ', '-'], "_"),
    }
}
