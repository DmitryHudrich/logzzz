use async_trait::async_trait;
use clickhouse::Client;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use crate::domain::record::{AccountRecord, FileHash};
use crate::domain::repository::{
    CredentialRepository, GroupedCredential, RepositoryError, RepositoryResult, SearchQuery,
    SearchType, SourceFilePathRecord, SourceFileRecord, StoreMetrics,
};

impl From<clickhouse::error::Error> for RepositoryError {
    fn from(error: clickhouse::error::Error) -> Self {
        RepositoryError::Backend(error.to_string())
    }
}

#[derive(Debug, Serialize, clickhouse::Row)]
struct CredRow {
    file_hash: String,
    source_file: String,
    username_raw: String,
    url_raw: String,
    password_raw: String,
    extra_json: String,
}

#[derive(Debug, Serialize, clickhouse::Row, Clone)]
struct SourceFilePathRow {
    file_hash: String,
    path: PathBuf,
    modified_at: Option<u32>,
    file_size: u64,
}

#[derive(Debug, Serialize, clickhouse::Row, Clone)]
struct SourceFileRow {
    file_hash: String,
    file_size: u64,
    parse_status: String,
    error_message: Option<String>,
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

#[derive(Debug, Deserialize, clickhouse::Row)]
struct GroupedCredRow {
    url_raw: String,
    username_raw: String,
    password_raw: String,
    extra_json: String,
    source_files: Vec<String>,
    file_hashes: Vec<String>,
}

#[derive(Debug, Deserialize, clickhouse::Row)]
struct PathRow {
    file_hash: String,
    path: String,
}

#[derive(Deserialize, clickhouse::Row)]
struct CountRow {
    #[serde(rename = "count()")]
    count: u64,
}

#[derive(Debug, Serialize, clickhouse::Row)]
struct CredTagRow {
    cred_key: String,
    tag: String,
    deleted: u8,
    version: u64,
}

#[derive(Debug, Deserialize, clickhouse::Row)]
struct CredKeyTagRow {
    cred_key: String,
    tag: String,
}

#[derive(Debug, Deserialize, clickhouse::Row)]
struct TagRow {
    tag: String,
}

#[derive(Clone)]
pub struct ClickhouseCredentialRepository {
    client: Arc<Client>,
}

impl ClickhouseCredentialRepository {
    pub fn new(client: Arc<Client>) -> Self {
        Self { client }
    }

    async fn write_tags(
        &self,
        cred_key: &str,
        tags: &[String],
        deleted: u8,
    ) -> RepositoryResult<()> {
        let normalized: Vec<String> = tags
            .iter()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        if normalized.is_empty() {
            return Ok(());
        }

        let version = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

        let mut insert = self.client.insert::<CredTagRow>("cred_tags").await?;
        for tag in normalized {
            insert
                .write(&CredTagRow {
                    cred_key: cred_key.to_string(),
                    tag,
                    deleted,
                    version,
                })
                .await?;
        }
        insert.end().await?;
        Ok(())
    }
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

fn where_clause_for(search_type: SearchType) -> &'static str {
    match search_type {
        SearchType::Login => "lower(username_raw) LIKE ?",
        SearchType::Url => "lower(url_raw) LIKE ?",
    }
}

const CRED_KEY_EXPR: &str =
    "lowerUTF8(hex(SHA256(concat(url_raw, '\n', username_raw, '\n', password_raw))))";

fn complete_clause(require_complete: bool) -> &'static str {
    if require_complete {
        " AND url_raw != '' AND username_raw != '' AND password_raw != ''"
    } else {
        ""
    }
}

fn tag_filter_clause(tags: &[String]) -> String {
    if tags.is_empty() {
        String::new()
    } else {
        format!(
            " AND {CRED_KEY_EXPR} IN (
                 SELECT cred_key FROM cred_tags FINAL
                 WHERE tag IN ? AND deleted = 0
                 GROUP BY cred_key
                 HAVING uniqExact(tag) = ?
             )"
        )
    }
}

fn exclude_tag_clause(exclude_tags: &[String]) -> String {
    if exclude_tags.is_empty() {
        String::new()
    } else {
        format!(
            " AND {CRED_KEY_EXPR} NOT IN (
                 SELECT cred_key FROM cred_tags FINAL
                 WHERE tag IN ? AND deleted = 0
             )"
        )
    }
}

#[async_trait]
impl CredentialRepository for ClickhouseCredentialRepository {
    async fn insert_records(&self, records: &[AccountRecord]) -> RepositoryResult<usize> {
        let rows: Vec<CredRow> = records.iter().filter_map(to_clickhouse_row).collect();
        if rows.is_empty() {
            return Ok(0);
        }

        let mut insert = self.client.insert::<CredRow>("creds").await?;
        for row in &rows {
            insert.write(row).await?;
        }
        insert.end().await?;
        Ok(rows.len())
    }

    async fn load_parsed_hashes(&self) -> RepositoryResult<HashSet<FileHash>> {
        let rows = self
            .client
            .query("SELECT file_hash FROM source_files")
            .fetch_all::<ExistingHashRow>()
            .await?;

        Ok(rows.into_iter().map(|r| FileHash(r.file_hash)).collect())
    }

    async fn load_seen_paths(&self) -> RepositoryResult<HashMap<PathBuf, HashSet<FileHash>>> {
        let rows = self
            .client
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

    async fn record_source_files(&self, rows: &[SourceFileRecord]) -> RepositoryResult<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut insert = self.client.insert::<SourceFileRow>("source_files").await?;
        for row in rows {
            insert
                .write(&SourceFileRow {
                    file_hash: row.file_hash.clone(),
                    file_size: row.file_size,
                    parse_status: row.parse_status.clone(),
                    error_message: row.error_message.clone(),
                })
                .await?;
        }
        insert.end().await?;
        Ok(())
    }

    async fn record_source_file_paths(
        &self,
        rows: &[SourceFilePathRecord],
    ) -> RepositoryResult<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut insert = self
            .client
            .insert::<SourceFilePathRow>("source_file_paths")
            .await?;
        for row in rows {
            insert
                .write(&SourceFilePathRow {
                    file_hash: row.file_hash.clone(),
                    path: row.path.clone(),
                    modified_at: row.modified_at,
                    file_size: row.file_size,
                })
                .await?;
        }
        insert.end().await?;
        Ok(())
    }

    async fn search_grouped(&self, query: &SearchQuery) -> RepositoryResult<Vec<GroupedCredential>> {
        let pattern = format!("%{}%", query.term.to_lowercase());
        let sql = format!(
            "SELECT
                 url_raw,
                 username_raw,
                 password_raw,
                 any(extra_json)             AS extra_json,
                 groupUniqArray(source_file) AS source_files,
                 groupUniqArray(file_hash)   AS file_hashes
             FROM creds
             WHERE {where_clause}{complete}{tag_clause}{exclude_clause}
             GROUP BY url_raw, username_raw, password_raw
             ORDER BY url_raw, username_raw
             LIMIT ? OFFSET ?",
            where_clause = where_clause_for(query.search_type),
            complete = complete_clause(query.require_complete),
            tag_clause = tag_filter_clause(&query.tags),
            exclude_clause = exclude_tag_clause(&query.exclude_tags),
        );

        let mut request = self.client.query(&sql).bind(&pattern);
        if !query.tags.is_empty() {
            request = request.bind(&query.tags).bind(query.tags.len() as u64);
        }
        if !query.exclude_tags.is_empty() {
            request = request.bind(&query.exclude_tags);
        }
        let rows = request
            .bind(query.limit as u64)
            .bind(query.offset as u64)
            .fetch_all::<GroupedCredRow>()
            .await?;

        Ok(rows
            .into_iter()
            .map(|r| GroupedCredential {
                url: r.url_raw,
                username: r.username_raw,
                password: r.password_raw,
                extra_json: r.extra_json,
                source_files: r.source_files,
                file_hashes: r.file_hashes,
            })
            .collect())
    }

    async fn count_grouped(&self, query: &SearchQuery) -> RepositoryResult<u64> {
        let pattern = format!("%{}%", query.term.to_lowercase());
        let sql = format!(
            "SELECT count() FROM (
                 SELECT 1 FROM creds WHERE {where_clause}{complete}{tag_clause}{exclude_clause}
                 GROUP BY url_raw, username_raw, password_raw
             )",
            where_clause = where_clause_for(query.search_type),
            complete = complete_clause(query.require_complete),
            tag_clause = tag_filter_clause(&query.tags),
            exclude_clause = exclude_tag_clause(&query.exclude_tags),
        );

        let mut request = self.client.query(&sql).bind(&pattern);
        if !query.tags.is_empty() {
            request = request.bind(&query.tags).bind(query.tags.len() as u64);
        }
        if !query.exclude_tags.is_empty() {
            request = request.bind(&query.exclude_tags);
        }
        let rows = request.fetch_all::<CountRow>().await?;

        Ok(rows.first().map(|r| r.count).unwrap_or(0))
    }

    async fn add_tags(&self, cred_key: &str, tags: &[String]) -> RepositoryResult<()> {
        self.write_tags(cred_key, tags, 0).await
    }

    async fn remove_tags(&self, cred_key: &str, tags: &[String]) -> RepositoryResult<()> {
        self.write_tags(cred_key, tags, 1).await
    }

    async fn tags_for_keys(&self, keys: &[String]) -> RepositoryResult<HashMap<String, Vec<String>>> {
        if keys.is_empty() {
            return Ok(HashMap::new());
        }

        let rows = self
            .client
            .query(
                "SELECT cred_key, tag FROM cred_tags FINAL
                 WHERE cred_key IN ? AND deleted = 0
                 ORDER BY cred_key, tag",
            )
            .bind(keys)
            .fetch_all::<CredKeyTagRow>()
            .await?;

        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for row in rows {
            map.entry(row.cred_key).or_default().push(row.tag);
        }
        Ok(map)
    }

    async fn all_tags(&self) -> RepositoryResult<Vec<String>> {
        let rows = self
            .client
            .query(
                "SELECT tag FROM cred_tags FINAL
                 WHERE deleted = 0
                 GROUP BY tag
                 ORDER BY tag",
            )
            .fetch_all::<TagRow>()
            .await?;

        Ok(rows.into_iter().map(|r| r.tag).collect())
    }

    async fn paths_for_hashes(&self, hashes: &[String]) -> RepositoryResult<HashMap<String, Vec<String>>> {
        if hashes.is_empty() {
            return Ok(HashMap::new());
        }

        let rows = self
            .client
            .query("SELECT file_hash, path FROM source_file_paths WHERE file_hash IN ? ORDER BY file_hash, path")
            .bind(hashes)
            .fetch_all::<PathRow>()
            .await?;

        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for row in rows {
            map.entry(row.file_hash).or_default().push(row.path);
        }

        Ok(map)
    }

    async fn metrics(&self) -> RepositoryResult<StoreMetrics> {
        let creds = self
            .client
            .query("SELECT count() FROM creds")
            .fetch_all::<CountRow>()
            .await?
            .first()
            .map(|r| r.count)
            .unwrap_or(0);

        let source_files = self
            .client
            .query("SELECT count() FROM source_files")
            .fetch_all::<CountRow>()
            .await?
            .first()
            .map(|r| r.count)
            .unwrap_or(0);

        Ok(StoreMetrics {
            total_credentials: creds,
            total_source_files: source_files,
        })
    }
}
