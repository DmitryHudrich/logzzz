use async_trait::async_trait;
use std::path::{Path, PathBuf};
use tokio::fs;

use crate::domain::source::{ArchiveInbox, SourceError};
use crate::infrastructure::archive::{detect_archive_kind, sanitize_filename};

#[derive(Clone)]
pub struct FsArchiveInbox {
    archive_dir: PathBuf,
}

impl FsArchiveInbox {
    pub fn new(archive_dir: impl Into<PathBuf>) -> Self {
        Self {
            archive_dir: archive_dir.into(),
        }
    }

    fn unique_name(&self, original_name: &str, discriminator: &str) -> String {
        let safe = sanitize_filename(original_name);
        let extension = Path::new(&safe)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());

        match extension {
            Some(ext) => {
                let stem = safe
                    .strip_suffix(&format!(".{ext}"))
                    .unwrap_or(&safe)
                    .to_string();
                format!("{stem}-{discriminator}.{ext}")
            }
            None => format!("{safe}-{discriminator}"),
        }
    }
}

#[async_trait]
impl ArchiveInbox for FsArchiveInbox {
    async fn deposit(&self, src: &Path, original_name: &str) -> Result<PathBuf, SourceError> {
        if detect_archive_kind(Path::new(original_name)).is_none() {
            return Err(SourceError::Other(format!(
                "unsupported archive type for `{original_name}`"
            )));
        }

        fs::create_dir_all(&self.archive_dir).await?;

        let discriminator = format!(
            "{}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
            rand::random::<u32>()
        );
        let final_path = self
            .archive_dir
            .join(self.unique_name(original_name, &discriminator));

        match fs::rename(src, &final_path).await {
            Ok(()) => Ok(final_path),
            Err(_) => {
                fs::copy(src, &final_path).await?;
                Ok(final_path)
            }
        }
    }
}
