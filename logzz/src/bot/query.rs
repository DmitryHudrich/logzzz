use clickhouse::Client;
use eyre::Result;
use serde::Deserialize;
use std::collections::HashMap;

use super::types::{CredRecord, GroupedCredRow};

pub async fn fetch_grouped(
    client: &Client,
    query: &str,
    search_type: &str,
    limit: usize,
    offset: usize,
) -> Result<Vec<GroupedCredRow>> {
    let pattern = format!("%{}%", query.to_lowercase());
    let where_clause = match search_type {
        "login" => "lower(username_raw) LIKE ?",
        _ => "lower(url_raw) LIKE ?",
    };
    let sql = format!(
        "SELECT
             url_raw,
             username_raw,
             password_raw,
             any(extra_json)             AS extra_json,
             groupUniqArray(source_file) AS source_files,
             groupUniqArray(file_hash)   AS file_hashes
         FROM creds
         WHERE {where_clause}
         GROUP BY url_raw, username_raw, password_raw
         ORDER BY url_raw, username_raw
         LIMIT ? OFFSET ?",
        where_clause = where_clause,
    );
    Ok(client
        .query(&sql)
        .bind(&pattern)
        .bind(limit as u64)
        .bind(offset as u64)
        .fetch_all::<GroupedCredRow>()
        .await?)
}

pub async fn fetch_total_count(client: &Client, query: &str, search_type: &str) -> Result<u64> {
    let pattern = format!("%{}%", query.to_lowercase());
    let where_clause = match search_type {
        "login" => "lower(username_raw) LIKE ?",
        _ => "lower(url_raw) LIKE ?",
    };
    let sql = format!(
        "SELECT count() FROM (
             SELECT 1 FROM creds WHERE {where_clause}
             GROUP BY url_raw, username_raw, password_raw
         )",
        where_clause = where_clause,
    );

    #[derive(Deserialize, clickhouse::Row)]
    struct CountRow {
        #[serde(rename = "count()")]
        count: u64,
    }

    let rows = client
        .query(&sql)
        .bind(&pattern)
        .fetch_all::<CountRow>()
        .await?;

    Ok(rows.first().map(|r| r.count).unwrap_or(0))
}

pub async fn fetch_all_paths(
    client: &Client,
    hashes: &[String],
) -> Result<HashMap<String, Vec<String>>> {
    if hashes.is_empty() {
        return Ok(HashMap::new());
    }

    let in_list: String = hashes
        .iter()
        .map(|h| format!("'{}'", h.replace('\'', "\\'")))
        .collect::<Vec<_>>()
        .join(", ");

    let sql = format!(
        "SELECT file_hash, path FROM source_file_paths WHERE file_hash IN ({in_list}) ORDER BY file_hash, path",
        in_list = in_list,
    );

    #[derive(Debug, Deserialize, clickhouse::Row)]
    struct PathRow {
        file_hash: String,
        path: String,
    }

    let rows = client.query(&sql).fetch_all::<PathRow>().await?;
    let mut map: HashMap<String, Vec<String>> = HashMap::new();

    for row in rows {
        map.entry(row.file_hash).or_default().push(row.path);
    }

    Ok(map)
}

pub fn build_record(
    row: &GroupedCredRow,
    hash_to_paths: &HashMap<String, Vec<String>>,
) -> CredRecord {
    let mut all_paths: Vec<String> = row
        .file_hashes
        .iter()
        .flat_map(|h| hash_to_paths.get(h).cloned().unwrap_or_default())
        .collect();

    for p in &row.source_files {
        if !all_paths.contains(p) {
            all_paths.push(p.clone());
        }
    }

    all_paths.sort();
    all_paths.dedup();

    let primary_path = all_paths.first().cloned().unwrap_or_default();

    CredRecord {
        url: row.url_raw.clone(),
        username: row.username_raw.clone(),
        password: row.password_raw.clone(),
        extra_json: row.extra_json.clone(),
        primary_path,
        all_paths,
    }
}
