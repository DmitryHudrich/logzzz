use serde::Deserialize;

#[derive(Debug, Deserialize, clickhouse::Row)]
pub struct GroupedCredRow {
    pub url_raw: String,
    pub username_raw: String,
    pub password_raw: String,
    pub extra_json: String,
    pub source_files: Vec<String>,
    pub file_hashes: Vec<String>,
}

pub struct CredRecord {
    pub url: String,
    pub username: String,
    pub password: String,
    pub extra_json: String,
    pub primary_path: String,
    pub all_paths: Vec<String>,
}
