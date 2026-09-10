use async_trait::async_trait;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::domain::source::{ArchiveInbox, LogSource, SourceError};
use crate::infrastructure::archive::is_archive_file;

pub struct LocalDirectorySource {
    name: String,
    watch_dir: PathBuf,
    state_file: PathBuf,
    processed: Mutex<HashSet<String>>,
}

impl LocalDirectorySource {
    pub async fn new(
        name: impl Into<String>,
        watch_dir: impl Into<PathBuf>,
        state_file: impl Into<PathBuf>,
    ) -> Self {
        let state_file = state_file.into();
        let processed = load_state(&state_file).await;
        Self {
            name: name.into(),
            watch_dir: watch_dir.into(),
            state_file,
            processed: Mutex::new(processed),
        }
    }

    async fn persist(&self, processed: &HashSet<String>) {
        if let Some(parent) = self.state_file.parent()
            && !parent.as_os_str().is_empty()
            && let Err(error) = fs::create_dir_all(parent).await
        {
            warn!(error = %error, "failed to create local source state dir");
            return;
        }

        let entries: Vec<&String> = processed.iter().collect();
        match serde_json::to_vec_pretty(&entries) {
            Ok(bytes) => {
                let tmp = self.state_file.with_extension("tmp");
                if let Err(error) = fs::write(&tmp, bytes).await {
                    warn!(error = %error, "failed to write local source state");
                    return;
                }
                if let Err(error) = fs::rename(&tmp, &self.state_file).await {
                    warn!(error = %error, "failed to persist local source state");
                }
            }
            Err(error) => warn!(error = %error, "failed to serialize local source state"),
        }
    }
}

fn identity(path: &Path, size: u64, modified: u64) -> String {
    format!("{}|{}|{}", path.display(), size, modified)
}

async fn load_state(state_file: &Path) -> HashSet<String> {
    match fs::read(state_file).await {
        Ok(bytes) => serde_json::from_slice::<Vec<String>>(&bytes)
            .map(|v| v.into_iter().collect())
            .unwrap_or_default(),
        Err(_) => HashSet::new(),
    }
}

#[async_trait]
impl LogSource for LocalDirectorySource {
    fn name(&self) -> &str {
        &self.name
    }

    async fn poll(&self, inbox: &dyn ArchiveInbox) -> Result<usize, SourceError> {
        let mut reader = match fs::read_dir(&self.watch_dir).await {
            Ok(reader) => reader,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                debug!(source = %self.name, dir = %self.watch_dir.display(), "watch dir missing");
                return Ok(0);
            }
            Err(error) => return Err(error.into()),
        };

        let mut candidates = Vec::new();
        while let Some(entry) = reader.next_entry().await? {
            let path = entry.path();
            if !is_archive_file(&path) {
                continue;
            }

            let metadata = match entry.metadata().await {
                Ok(metadata) => metadata,
                Err(error) => {
                    warn!(error = %error, path = %path.display(), "failed to stat archive");
                    continue;
                }
            };
            let modified = metadata
                .modified()
                .ok()
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            candidates.push((path, metadata.len(), modified));
        }

        candidates.sort_by(|a, b| a.0.cmp(&b.0));

        let mut deposited = 0usize;
        let mut processed = self.processed.lock().await;

        for (path, size, modified) in candidates {
            let id = identity(&path, size, modified);
            if processed.contains(&id) {
                continue;
            }

            let original_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("archive");

            match inbox.deposit(&path, original_name).await {
                Ok(final_path) => {
                    debug!(
                        source = %self.name,
                        from = %path.display(),
                        to = %final_path.display(),
                        "deposited archive from local directory"
                    );
                    processed.insert(id);
                    deposited += 1;
                }
                Err(error) => {
                    warn!(error = %error, path = %path.display(), "failed to deposit local archive");
                }
            }
        }

        if deposited > 0 {
            self.persist(&processed).await;
        }

        Ok(deposited)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::inbox::FsArchiveInbox;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_temp_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("logzz-src-{label}-{}-{n}", std::process::id()))
    }

    #[tokio::test]
    async fn deposits_new_archives_once_and_dedupes_on_repoll() {
        let watch = unique_temp_dir("watch");
        let archive = unique_temp_dir("archive");
        let state = unique_temp_dir("state").join("local.state.json");
        fs::create_dir_all(&watch).await.unwrap();
        fs::create_dir_all(&archive).await.unwrap();
        fs::write(watch.join("dump.zip"), b"pk-not-a-real-zip")
            .await
            .unwrap();
        fs::write(watch.join("notes.txt"), b"ignored").await.unwrap();

        let inbox: Box<dyn ArchiveInbox> = Box::new(FsArchiveInbox::new(&archive));
        let source = LocalDirectorySource::new("local:test", &watch, &state).await;

        let first = source.poll(inbox.as_ref()).await.unwrap();
        assert_eq!(first, 1);

        let deposited: Vec<_> = std::fs::read_dir(&archive)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| is_archive_file(p))
            .collect();
        assert_eq!(deposited.len(), 1);

        let reloaded = LocalDirectorySource::new("local:test", &watch, &state).await;
        let second = reloaded.poll(inbox.as_ref()).await.unwrap();
        assert_eq!(second, 0);

        let _ = fs::remove_dir_all(&watch).await;
        let _ = fs::remove_dir_all(&archive).await;
    }
}
