use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::io;

use crate::Result;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct DownloaderState {
    pub last_downloaded_archive_message_id: i32,
}

pub async fn load_state(path: &Path) -> Result<DownloaderState> {
    match tokio::fs::read(path).await {
        Ok(raw) => Ok(serde_json::from_slice(&raw)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(DownloaderState::default()),
        Err(error) => Err(Box::new(error)),
    }
}

pub async fn save_state(path: &Path, state: &DownloaderState) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent).await?;
    }

    let temp_path = path.with_extension("tmp");
    let payload = serde_json::to_vec_pretty(state)?;
    tokio::fs::write(&temp_path, payload).await?;
    tokio::fs::rename(&temp_path, path).await?;
    Ok(())
}
