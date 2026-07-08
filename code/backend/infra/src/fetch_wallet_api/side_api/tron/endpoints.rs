fn push_window(s: &mut String, min_ts: Option<u64>, max_ts: Option<u64>) {
    if let Some(t) = min_ts {
        s.push_str("&min_timestamp=");
        s.push_str(&t.to_string());
    }
    if let Some(t) = max_ts {
        s.push_str("&max_timestamp=");
        s.push_str(&t.to_string());
    }
}

pub fn trc20_transfers(
    address_b58: &str,
    fingerprint: Option<&str>,
    min_ts: Option<u64>,
    max_ts: Option<u64>,
) -> String {
    let mut s = format!(
        "/v1/accounts/{address_b58}/transactions/trc20?limit=200&only_confirmed=true"
    );
    if let Some(fp) = fingerprint {
        s.push_str("&fingerprint=");
        s.push_str(fp);
    }
    push_window(&mut s, min_ts, max_ts);
    s
}

pub fn trc20_transfers_for_token(
    address_b58: &str,
    contract_address_b58: &str,
    fingerprint: Option<&str>,
) -> String {
    let mut s = format!(
        "/v1/accounts/{address_b58}/transactions/trc20?contract_address={contract_address_b58}&limit=200&only_confirmed=true"
    );
    if let Some(fp) = fingerprint {
        s.push_str("&fingerprint=");
        s.push_str(fp);
    }
    s
}

pub fn native_transfers(
    address_b58: &str,
    fingerprint: Option<&str>,
    min_ts: Option<u64>,
    max_ts: Option<u64>,
) -> String {
    let mut s = format!(
        "/v1/accounts/{address_b58}/transactions?limit=200&only_confirmed=true"
    );
    if let Some(fp) = fingerprint {
        s.push_str("&fingerprint=");
        s.push_str(fp);
    }
    push_window(&mut s, min_ts, max_ts);
    s
}
