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
    ingest_time   DateTime DEFAULT now(),
    file_hash     String,
    source_file   LowCardinality(String),

    username_raw  String,
    url_raw       String,

    host_full     LowCardinality(String) MATERIALIZED lowerUTF8(domainRFC(url_raw)),
    host_no_www   LowCardinality(String) MATERIALIZED lowerUTF8(domainWithoutWWWRFC(url_raw)),
    host_root     LowCardinality(String) MATERIALIZED lowerUTF8(cutToFirstSignificantSubdomainRFC(url_raw)),

    password_raw  String,
    extra_json    String
)
ENGINE = MergeTree
PARTITION BY toYYYYMM(ingest_time)
ORDER BY (host_root, ingest_time);

CREATE TABLE IF NOT EXISTS logzz.source_files
(
    file_hash      String,
    file_size      UInt64,
    parse_status   LowCardinality(String),
    error_message  Nullable(String)
)
ENGINE = MergeTree
ORDER BY file_hash;

CREATE TABLE IF NOT EXISTS logzz.source_file_paths
(
    discovered_at  DateTime DEFAULT now(),
    file_hash      String,
    path           String,
    modified_at    Nullable(DateTime),
    file_size      UInt64
)
ENGINE = MergeTree
PARTITION BY toYYYYMM(discovered_at)
ORDER BY (file_hash, path, discovered_at);
