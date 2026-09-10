CREATE DATABASE IF NOT EXISTS logzz;

CREATE TABLE IF NOT EXISTS logzz.schema_migrations
(
    version String,
    applied_at DateTime DEFAULT now()
)
ENGINE = MergeTree
ORDER BY version;

CREATE TABLE IF NOT EXISTS logzz.creds
(
    ingest_time   DateTime DEFAULT now() CODEC(DoubleDelta, ZSTD(1)),
    file_hash     String CODEC(ZSTD(3)),
    source_file   LowCardinality(String) CODEC(ZSTD(1)),

    username_raw  String CODEC(ZSTD(3)),
    url_raw       String CODEC(ZSTD(3)),

    host_full     LowCardinality(String) MATERIALIZED lowerUTF8(domainRFC(url_raw)),
    host_no_www   LowCardinality(String) MATERIALIZED lowerUTF8(domainWithoutWWWRFC(url_raw)),
    host_root     LowCardinality(String) MATERIALIZED lowerUTF8(cutToFirstSignificantSubdomainRFC(url_raw)),

    password_raw  String CODEC(ZSTD(3)),
    extra_json    String CODEC(ZSTD(3))
)
ENGINE = MergeTree
PARTITION BY toYYYYMM(ingest_time)
ORDER BY (host_root, url_raw, username_raw, password_raw, ingest_time);

CREATE TABLE IF NOT EXISTS logzz.source_files
(
    file_hash      String CODEC(ZSTD(3)),
    file_size      UInt64 CODEC(T64, ZSTD(1)),
    parse_status   LowCardinality(String) CODEC(ZSTD(1)),
    error_message  Nullable(String) CODEC(ZSTD(3))
)
ENGINE = MergeTree
ORDER BY file_hash;

CREATE TABLE IF NOT EXISTS logzz.source_file_paths
(
    discovered_at  DateTime DEFAULT now() CODEC(DoubleDelta, ZSTD(1)),
    file_hash      String CODEC(ZSTD(3)),
    path           String CODEC(ZSTD(3)),
    modified_at    Nullable(DateTime) CODEC(DoubleDelta, ZSTD(1)),
    file_size      UInt64 CODEC(T64, ZSTD(1))
)
ENGINE = MergeTree
PARTITION BY toYYYYMM(discovered_at)
ORDER BY (file_hash, path, discovered_at);
