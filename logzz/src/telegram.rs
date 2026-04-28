use eyre::Result;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::path::{Path, PathBuf};
use tokio::fs;

use crate::archive::sanitize_filename;

pub const ARCHIVE_UPLOAD_REQUEST_SUFFIX: &str = ".logzz-upload.json";
const PENDING_NOTIFICATION_DIR: &str = ".logzz-telegram";

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ArchiveUploadRequest {
    #[serde(default)]
    pub chat_id: Option<i64>,
    pub message_id: i32,
    pub original_name: String,
    #[serde(default)]
    pub userbot_peer_name: Option<String>,
    #[serde(default)]
    pub progress_message_id: Option<i32>,
}

impl ArchiveUploadRequest {
    pub fn for_bot(chat_id: i64, message_id: i32, original_name: impl Into<String>) -> Self {
        Self {
            chat_id: Some(chat_id),
            message_id,
            original_name: original_name.into(),
            userbot_peer_name: None,
            progress_message_id: None,
        }
    }

    pub fn for_userbot(
        peer_name: impl Into<String>,
        message_id: i32,
        progress_message_id: i32,
        original_name: impl Into<String>,
    ) -> Self {
        Self {
            chat_id: None,
            message_id,
            original_name: original_name.into(),
            userbot_peer_name: Some(peer_name.into()),
            progress_message_id: Some(progress_message_id),
        }
    }

    pub fn bot_chat_id(&self) -> Option<i64> {
        self.chat_id
    }

    pub fn userbot_progress_message_id(&self, peer_name: &str) -> Option<i32> {
        match (&self.userbot_peer_name, self.progress_message_id) {
            (Some(target_peer_name), Some(progress_message_id))
                if target_peer_name == peer_name =>
            {
                Some(progress_message_id)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Eq, PartialEq)]
pub struct ArchiveParseSummary {
    pub files_parsed: usize,
    pub files_skipped: usize,
    pub records_inserted: usize,
    pub issues_found: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct PendingArchiveNotification {
    pub request: ArchiveUploadRequest,
    pub output_dir: Option<PathBuf>,
    pub summary: Option<ArchiveParseSummary>,
}

impl PendingArchiveNotification {
    pub fn pending_parse(request: ArchiveUploadRequest, output_dir: PathBuf) -> Self {
        Self {
            request,
            output_dir: Some(output_dir),
            summary: None,
        }
    }

    pub fn mark_ready(&mut self, summary: ArchiveParseSummary) {
        self.output_dir = None;
        self.summary = Some(summary);
    }

    pub fn is_ready(&self) -> bool {
        self.summary.is_some()
    }
}

pub fn upload_request_path(archive_path: &Path) -> PathBuf {
    let file_name = archive_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| format!("{name}{ARCHIVE_UPLOAD_REQUEST_SUFFIX}"))
        .unwrap_or_else(|| format!("archive{ARCHIVE_UPLOAD_REQUEST_SUFFIX}"));

    archive_path.with_file_name(file_name)
}

pub fn archive_path_from_upload_request(request_path: &Path) -> Option<PathBuf> {
    let file_name = request_path.file_name()?.to_str()?;
    let archive_name = file_name.strip_suffix(ARCHIVE_UPLOAD_REQUEST_SUFFIX)?;
    Some(request_path.with_file_name(archive_name))
}

pub async fn write_upload_request(
    archive_path: &Path,
    request: &ArchiveUploadRequest,
) -> Result<()> {
    write_json_file(&upload_request_path(archive_path), request).await
}

pub async fn load_upload_request(archive_path: &Path) -> Result<Option<ArchiveUploadRequest>> {
    load_json_file(&upload_request_path(archive_path)).await
}

pub async fn load_upload_request_file(request_path: &Path) -> Result<Option<ArchiveUploadRequest>> {
    load_json_file(request_path).await
}

pub async fn remove_upload_request(archive_path: &Path) -> Result<()> {
    remove_optional_file(&upload_request_path(archive_path)).await
}

pub fn pending_notifications_dir(archive_dir: &Path) -> PathBuf {
    archive_dir.join(PENDING_NOTIFICATION_DIR)
}

pub fn pending_notification_path(archive_dir: &Path, request: &ArchiveUploadRequest) -> PathBuf {
    let file_name = if let Some(chat_id) = request.chat_id {
        format!("bot-{chat_id}-{:010}.json", request.message_id)
    } else if let (Some(peer_name), Some(progress_message_id)) =
        (&request.userbot_peer_name, request.progress_message_id)
    {
        format!(
            "userbot-{}-{:010}-{:010}.json",
            sanitize_filename(peer_name),
            request.message_id,
            progress_message_id
        )
    } else {
        format!("unknown-{:010}.json", request.message_id)
    };

    pending_notifications_dir(archive_dir).join(file_name)
}

pub async fn queue_pending_parse_notification(
    archive_dir: &Path,
    request: ArchiveUploadRequest,
    output_dir: PathBuf,
) -> Result<PathBuf> {
    let path = pending_notification_path(archive_dir, &request);
    let notification = PendingArchiveNotification::pending_parse(request, output_dir);
    save_pending_notification(&path, &notification).await?;
    Ok(path)
}

pub async fn save_pending_notification(
    path: &Path,
    notification: &PendingArchiveNotification,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }

    write_json_file(path, notification).await
}

pub async fn load_pending_notifications(
    archive_dir: &Path,
) -> Result<Vec<(PathBuf, PendingArchiveNotification)>> {
    let dir = pending_notifications_dir(archive_dir);
    let mut reader = match fs::read_dir(&dir).await {
        Ok(reader) => reader,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(error.into()),
    };

    let mut rows = Vec::new();
    while let Some(entry) = reader.next_entry().await? {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        if let Some(notification) = load_json_file(&path).await? {
            rows.push((path, notification));
        }
    }

    rows.sort_by(|(lhs, _), (rhs, _)| lhs.cmp(rhs));
    Ok(rows)
}

pub fn format_ready_notification(notification: &PendingArchiveNotification) -> Option<String> {
    let summary = notification.summary.as_ref()?;

    Some(format!(
        "Archive parsing finished: {}\nFiles parsed: {}\nFiles skipped: {}\nRecords inserted: {}\nIssues found: {}",
        notification.request.original_name,
        summary.files_parsed,
        summary.files_skipped,
        summary.records_inserted,
        summary.issues_found
    ))
}

async fn write_json_file<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    let bytes = serde_json::to_vec(value)?;
    fs::write(path, bytes).await?;
    Ok(())
}

async fn load_json_file<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::read(path).await {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn remove_optional_file(path: &Path) -> Result<()> {
    match fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ArchiveParseSummary, ArchiveUploadRequest, PendingArchiveNotification,
        archive_path_from_upload_request, format_ready_notification, upload_request_path,
    };
    use std::path::Path;

    #[test]
    fn upload_request_path_round_trips_back_to_archive_path() {
        let archive_path = Path::new("/tmp/0000000123-sample.zip");
        let request_path = upload_request_path(archive_path);

        assert_eq!(
            archive_path_from_upload_request(&request_path),
            Some(archive_path.to_path_buf())
        );
    }

    #[test]
    fn ready_notification_contains_summary_counts() {
        let mut notification = PendingArchiveNotification::pending_parse(
            ArchiveUploadRequest::for_bot(42, 7, "sample.zip"),
            Path::new("/tmp/out").to_path_buf(),
        );

        notification.mark_ready(ArchiveParseSummary {
            files_parsed: 3,
            files_skipped: 2,
            records_inserted: 17,
            issues_found: 1,
        });

        let message = format_ready_notification(&notification).expect("message");
        assert!(message.contains("sample.zip"));
        assert!(message.contains("Files parsed: 3"));
        assert!(message.contains("Files skipped: 2"));
        assert!(message.contains("Records inserted: 17"));
        assert!(message.contains("Issues found: 1"));
    }

    #[test]
    fn userbot_request_matches_only_its_peer() {
        let request = ArchiveUploadRequest::for_userbot("source_peer", 10, 77, "sample.zip");

        assert_eq!(request.userbot_progress_message_id("source_peer"), Some(77));
        assert_eq!(request.userbot_progress_message_id("other_peer"), None);
        assert_eq!(request.bot_chat_id(), None);
    }
}
