pub mod parser;
pub mod record;
pub mod repository;
pub mod source;
pub mod tag;

pub use parser::{Parser, parse_file};
pub use record::{AccountRecord, FileHash, ParseIssue, ParseReport, RawRecord};
pub use repository::{
    CredentialRepository, GroupedCredential, SearchQuery, SearchType, SourceFilePathRecord,
    SourceFileRecord, StoreMetrics,
};
pub use source::{ArchiveInbox, LogSource, SourceError};
pub use tag::credential_key;

#[derive(Debug, Clone)]
pub struct CredentialMatch {
    pub cred_key: String,
    pub url: String,
    pub username: String,
    pub password: String,
    pub extra_json: String,
    pub primary_path: String,
    pub all_paths: Vec<String>,
    pub tags: Vec<String>,
}
