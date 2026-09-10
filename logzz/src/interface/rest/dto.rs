use serde::Serialize;

use crate::application::ingest::ImportStatus;
use crate::domain::CredentialMatch;
use crate::domain::repository::StoreMetrics;

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
}

#[derive(Serialize)]
pub struct CredentialDto {
    pub cred_key: String,
    pub url: String,
    pub username: String,
    pub password: String,
    pub extra_json: String,
    pub primary_path: String,
    pub all_paths: Vec<String>,
    pub tags: Vec<String>,
}

impl From<CredentialMatch> for CredentialDto {
    fn from(m: CredentialMatch) -> Self {
        Self {
            cred_key: m.cred_key,
            url: m.url,
            username: m.username,
            password: m.password,
            extra_json: m.extra_json,
            primary_path: m.primary_path,
            all_paths: m.all_paths,
            tags: m.tags,
        }
    }
}

#[derive(Serialize)]
pub struct SearchResponse {
    pub query: String,
    pub search_type: String,
    pub tags: Vec<String>,
    pub page: usize,
    pub has_next: bool,
    pub total_unique: u64,
    pub count: usize,
    pub records: Vec<CredentialDto>,
}

#[derive(serde::Deserialize)]
pub struct TagsRequest {
    pub url: String,
    pub username: String,
    pub password: String,
    pub tags: Vec<String>,
}

#[derive(Serialize)]
pub struct TagListResponse {
    pub tags: Vec<String>,
}

#[derive(Serialize)]
pub struct OkResponse {
    pub ok: bool,
    pub cred_key: String,
    pub tags: Vec<String>,
}

#[derive(Serialize)]
pub struct MetricsResponse {
    pub total_credentials: u64,
    pub total_source_files: u64,
    pub import: ImportStatus,
    pub sources: Vec<String>,
}

impl MetricsResponse {
    pub fn new(store: StoreMetrics, import: ImportStatus, sources: Vec<String>) -> Self {
        Self {
            total_credentials: store.total_credentials,
            total_source_files: store.total_source_files,
            import,
            sources,
        }
    }
}

#[derive(Serialize)]
pub struct UploadResponse {
    pub queued: bool,
    pub path: String,
}

#[derive(Serialize)]
pub struct ApiError {
    pub ok: bool,
    pub message: String,
}
