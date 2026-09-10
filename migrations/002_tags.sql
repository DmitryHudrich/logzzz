CREATE TABLE IF NOT EXISTS logzz.cred_tags
(
    cred_key   String CODEC(ZSTD(3)),
    tag        String CODEC(ZSTD(3)),
    deleted    UInt8  DEFAULT 0 CODEC(ZSTD(1)),
    version    UInt64 CODEC(DoubleDelta, ZSTD(1))
)
ENGINE = ReplacingMergeTree(version)
ORDER BY (cred_key, tag);
