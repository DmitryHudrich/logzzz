CREATE TABLE IF NOT EXISTS logzz.cred_tags
(
    cred_key   String,
    tag        String,
    deleted    UInt8  DEFAULT 0,
    version    UInt64
)
ENGINE = ReplacingMergeTree(version)
ORDER BY (cred_key, tag);
