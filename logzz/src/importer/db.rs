use clickhouse::Client;
use eyre::Result;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use crate::{importer::FileHash, records::AccountRecord};

#[derive(Debug, Serialize, clickhouse::Row)]
pub struct CredRow {
    file_hash: String,
    source_file: String,
    username_raw: String,
    url_raw: String,
    password_raw: String,
    extra_json: String,
}

#[derive(Debug, Serialize, clickhouse::Row, Clone)]
pub struct SourceFilePathRow {
    pub file_hash: String,
    pub path: PathBuf,
    pub modified_at: Option<u32>,
    pub file_size: u64,
}

#[derive(Debug, Serialize, clickhouse::Row, Clone)]
pub struct SourceFileRow {
    pub file_hash: String,
    pub file_size: u64,
    pub parse_status: String,
    pub error_message: Option<String>,
}

#[derive(Debug, Deserialize, clickhouse::Row)]
struct ExistingPathRow {
    file_hash: String,
    path: PathBuf,
}

#[derive(Debug, Deserialize, clickhouse::Row)]
struct ExistingHashRow {
    file_hash: String,
}

pub async fn load_seen_paths(client: &Client) -> Result<HashMap<PathBuf, HashSet<FileHash>>> {
    let rows = client
        .query("SELECT file_hash, path FROM source_file_paths")
        .fetch_all::<ExistingPathRow>()
        .await?;

    let mut map: HashMap<PathBuf, HashSet<FileHash>> = HashMap::new();

    for row in rows {
        map.entry(row.path)
            .or_default()
            .insert(FileHash(row.file_hash));
    }

    Ok(map)
}

pub async fn load_parsed_hashes(client: &Client) -> Result<HashSet<FileHash>> {
    let rows = client
        .query("SELECT file_hash FROM source_files")
        .fetch_all::<ExistingHashRow>()
        .await?;

    Ok(rows.into_iter().map(|r| FileHash(r.file_hash)).collect())
}

pub async fn flush_source_file_paths(
    client: &Client,
    rows: &mut Vec<SourceFilePathRow>,
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }

    let mut insert = client
        .insert::<SourceFilePathRow>("source_file_paths")
        .await?;

    for row in rows.iter() {
        insert.write(row).await?;
    }

    insert.end().await?;
    rows.clear();
    Ok(())
}

pub async fn flush_source_files(client: &Client, rows: &mut Vec<SourceFileRow>) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }

    let mut insert = client.insert::<SourceFileRow>("source_files").await?;

    for row in rows.iter() {
        insert.write(row).await?;
    }

    insert.end().await?;
    rows.clear();
    Ok(())
}

fn to_clickhouse_row(rec: &AccountRecord) -> Option<CredRow> {
    let url_raw = rec.url()?.trim().to_string();
    if url_raw.is_empty() {
        return None;
    }

    Some(CredRow {
        file_hash: rec.file_hash().to_owned(),
        source_file: rec.source_file().to_owned(),
        username_raw: rec.username().cloned().unwrap_or_default(),
        url_raw,
        password_raw: rec.password().cloned().unwrap_or_default(),
        extra_json: serde_json::to_string(rec.extra()).ok()?,
    })
}

pub async fn insert_records(client: &Client, records: &[AccountRecord]) -> Result<usize> {
    let mut insert = client.insert::<CredRow>("creds").await?;
    let mut inserted = 0usize;

    for rec in records {
        if let Some(row) = to_clickhouse_row(rec) {
            insert.write(&row).await?;
            inserted += 1;
        }
    }

    insert.end().await?;
    Ok(inserted)
}
