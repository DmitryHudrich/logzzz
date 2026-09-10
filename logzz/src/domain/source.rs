use async_trait::async_trait;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("source i/o error: {0}")]
    Io(String),
    #[error("source error: {0}")]
    Other(String),
}

impl From<std::io::Error> for SourceError {
    fn from(error: std::io::Error) -> Self {
        SourceError::Io(error.to_string())
    }
}

#[async_trait]
pub trait ArchiveInbox: Send + Sync {
    async fn deposit(&self, src: &Path, original_name: &str) -> Result<PathBuf, SourceError>;
}

#[async_trait]
pub trait LogSource: Send + Sync {
    fn name(&self) -> &str;

    async fn poll(&self, inbox: &dyn ArchiveInbox) -> Result<usize, SourceError>;
}
