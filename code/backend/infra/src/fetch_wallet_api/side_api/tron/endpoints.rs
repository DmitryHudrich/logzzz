fn push_window(s: &mut String, min_ts: Option<u64>, max_ts: Option<u64>) {
    if let Some(t) = min_ts {
        s.push_str("&start_timestamp=");
        s.push_str(&t.to_string());
    }
    if let Some(t) = max_ts {
        s.push_str("&end_timestamp=");
        s.push_str(&t.to_string());
    }
}

/// TRX (native) transaction list for an address.
///
/// Sorted ascending by timestamp (`sort=timestamp`, oldest first) rather than
/// the UI's default newest-first order — this source paginates by numeric
/// `start` offset, and Tronscan has no opaque cursor. With ascending order,
/// newly-confirmed transactions only ever append past the last page, so an
/// in-progress `start`-based walk stays stable; with descending order every
/// new confirmation would shift all older offsets by one and corrupt an
/// in-flight walk.
pub fn transactions(address_b58: &str, start: u32, limit: u32, min_ts: Option<u64>, max_ts: Option<u64>) -> String {
    let mut s = format!(
        "/api/transaction?sort=timestamp&count=false&limit={limit}&start={start}&address={address_b58}&confirm=true"
    );
    push_window(&mut s, min_ts, max_ts);
    s
}

/// TRC20 transfer list for an address. Same ascending-sort rationale as
/// `transactions` above.
pub fn trc20_transfers(address_b58: &str, start: u32, limit: u32, min_ts: Option<u64>, max_ts: Option<u64>) -> String {
    let mut s = format!(
        "/api/token_trc20/transfers?sort=timestamp&limit={limit}&start={start}&relatedAddress={address_b58}&confirm=true"
    );
    push_window(&mut s, min_ts, max_ts);
    s
}

/// Single-account detail: balance/type info plus, when Tronscan has curated
/// one, the account's public tag (`addressTag`/`addressTagLogo`) — e.g.
/// "Binance-Cold 2". Used for both `is_contract` (`accountType == 2`) and
/// `LabelProvider::resolve`.
pub fn account(address_b58: &str) -> String {
    format!("/api/account?address={address_b58}")
}

pub const LATEST_BLOCK: &str = "/api/block/latest";
