use sha2::{Digest, Sha256};

pub fn credential_key(url: &str, username: &str, password: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("{url}\n{username}\n{password}"));
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::credential_key;

    #[test]
    fn key_is_stable_and_lowercase_hex() {
        let key = credential_key("https://example.com", "alice", "hunter2");
        assert_eq!(key.len(), 64);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(key, credential_key("https://example.com", "alice", "hunter2"));
    }

    #[test]
    fn different_fields_produce_different_keys() {
        let a = credential_key("https://example.com", "alice", "hunter2");
        let b = credential_key("https://example.com", "alice", "hunter3");
        assert_ne!(a, b);
    }
}
