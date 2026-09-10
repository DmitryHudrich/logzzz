use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::domain::record::{AccountRecord, FileHash};

#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("repository backend error: {0}")]
    Backend(String),
}

pub type RepositoryResult<T> = Result<T, RepositoryError>;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SearchType {
    Url,
    Login,
}

impl SearchType {
    pub fn as_str(&self) -> &'static str {
        match self {
            SearchType::Url => "url",
            SearchType::Login => "login",
        }
    }

    pub fn from_str_lossy(value: &str) -> Self {
        match value {
            "login" => SearchType::Login,
            _ => SearchType::Url,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub term: String,
    pub search_type: SearchType,
    pub tags: Vec<String>,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Debug, Clone)]
pub struct GroupedCredential {
    pub url: String,
    pub username: String,
    pub password: String,
    pub extra_json: String,
    pub source_files: Vec<String>,
    pub file_hashes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SourceFileRecord {
    pub file_hash: String,
    pub file_size: u64,
    pub parse_status: String,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SourceFilePathRecord {
    pub file_hash: String,
    pub path: PathBuf,
    pub modified_at: Option<u32>,
    pub file_size: u64,
}

#[derive(Debug, Clone, Default)]
pub struct StoreMetrics {
    pub total_credentials: u64,
    pub total_source_files: u64,
}

#[async_trait]
pub trait CredentialRepository: Send + Sync {
    async fn insert_records(&self, records: &[AccountRecord]) -> RepositoryResult<usize>;
    async fn load_parsed_hashes(&self) -> RepositoryResult<HashSet<FileHash>>;
    async fn load_seen_paths(&self) -> RepositoryResult<HashMap<PathBuf, HashSet<FileHash>>>;
    async fn record_source_files(&self, rows: &[SourceFileRecord]) -> RepositoryResult<()>;
    async fn record_source_file_paths(
        &self,
        rows: &[SourceFilePathRecord],
    ) -> RepositoryResult<()>;

    async fn search_grouped(&self, query: &SearchQuery) -> RepositoryResult<Vec<GroupedCredential>>;
    async fn count_grouped(&self, query: &SearchQuery) -> RepositoryResult<u64>;
    async fn paths_for_hashes(
        &self,
        hashes: &[String],
    ) -> RepositoryResult<HashMap<String, Vec<String>>>;

    async fn add_tags(&self, cred_key: &str, tags: &[String]) -> RepositoryResult<()>;
    async fn remove_tags(&self, cred_key: &str, tags: &[String]) -> RepositoryResult<()>;
    async fn tags_for_keys(&self, keys: &[String])
    -> RepositoryResult<HashMap<String, Vec<String>>>;
    async fn all_tags(&self) -> RepositoryResult<Vec<String>>;

    async fn metrics(&self) -> RepositoryResult<StoreMetrics>;
}
