ALTER TABLE logzz.creds MODIFY COLUMN IF EXISTS ingest_time DateTime CODEC(DoubleDelta, ZSTD(1));
ALTER TABLE logzz.creds MODIFY COLUMN IF EXISTS file_hash String CODEC(ZSTD(3));
ALTER TABLE logzz.creds MODIFY COLUMN IF EXISTS source_file LowCardinality(String) CODEC(ZSTD(1));
ALTER TABLE logzz.creds MODIFY COLUMN IF EXISTS username_raw String CODEC(ZSTD(3));
ALTER TABLE logzz.creds MODIFY COLUMN IF EXISTS url_raw String CODEC(ZSTD(3));
ALTER TABLE logzz.creds MODIFY COLUMN IF EXISTS password_raw String CODEC(ZSTD(3));
ALTER TABLE logzz.creds MODIFY COLUMN IF EXISTS extra_json String CODEC(ZSTD(3));

ALTER TABLE logzz.source_files MODIFY COLUMN IF EXISTS file_hash String CODEC(ZSTD(3));
ALTER TABLE logzz.source_files MODIFY COLUMN IF EXISTS file_size UInt64 CODEC(T64, ZSTD(1));
ALTER TABLE logzz.source_files MODIFY COLUMN IF EXISTS parse_status LowCardinality(String) CODEC(ZSTD(1));
ALTER TABLE logzz.source_files MODIFY COLUMN IF EXISTS error_message Nullable(String) CODEC(ZSTD(3));

ALTER TABLE logzz.source_file_paths MODIFY COLUMN IF EXISTS discovered_at DateTime CODEC(DoubleDelta, ZSTD(1));
ALTER TABLE logzz.source_file_paths MODIFY COLUMN IF EXISTS file_hash String CODEC(ZSTD(3));
ALTER TABLE logzz.source_file_paths MODIFY COLUMN IF EXISTS path String CODEC(ZSTD(3));
ALTER TABLE logzz.source_file_paths MODIFY COLUMN IF EXISTS modified_at Nullable(DateTime) CODEC(DoubleDelta, ZSTD(1));
ALTER TABLE logzz.source_file_paths MODIFY COLUMN IF EXISTS file_size UInt64 CODEC(T64, ZSTD(1));

ALTER TABLE logzz.cred_tags MODIFY COLUMN IF EXISTS cred_key String CODEC(ZSTD(3));
ALTER TABLE logzz.cred_tags MODIFY COLUMN IF EXISTS tag String CODEC(ZSTD(3));
ALTER TABLE logzz.cred_tags MODIFY COLUMN IF EXISTS deleted UInt8 CODEC(ZSTD(1));
ALTER TABLE logzz.cred_tags MODIFY COLUMN IF EXISTS version UInt64 CODEC(DoubleDelta, ZSTD(1));
