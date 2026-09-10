use eyre::Result;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::application::events::{AppEvent, EventBus};
use crate::domain::parser::parse_file;
use crate::domain::record::{AccountRecord, FileHash};
use crate::domain::repository::{CredentialRepository, SourceFilePathRecord, SourceFileRecord};
use crate::infrastructure::archive::{
    ExtractError, archive_needs_password_path, archive_output_dir, archive_password_path,
    extract_archive, is_archive_file,
};
use crate::infrastructure::files::{file_hash, iter_files};
use crate::infrastructure::telegram_ipc::{
    ArchiveParseSummary, ArchiveUploadRequest, archive_path_from_upload_request,
    load_pending_notifications, load_upload_request, load_upload_request_file,
    queue_pending_parse_notification, remove_needs_password_marker, remove_upload_request,
    save_pending_notification, write_needs_password_marker,
};

#[derive(Debug, Default, Clone, Serialize)]
pub struct ImportCycleStats {
    pub archives_extracted: usize,
    pub files_parsed: usize,
    pub files_skipped: usize,
    pub records_parsed: usize,
    pub records_inserted: usize,
    pub issues_found: usize,
    pub notifications_queued: usize,
}

impl ImportCycleStats {
    pub fn did_work(&self) -> bool {
        self.archives_extracted > 0
            || self.files_parsed > 0
            || self.records_inserted > 0
            || self.notifications_queued > 0
    }
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct ImportStatus {
    pub cycles_completed: u64,
    pub last_cycle: ImportCycleStats,
    pub totals: ImportCycleStats,
    pub last_error: Option<String>,
    pub last_cycle_unix: Option<u64>,
    pub pending_archives: usize,
}

pub type SharedImportStatus = Arc<Mutex<ImportStatus>>;

impl ImportStatus {
    fn record(&mut self, stats: &ImportCycleStats) {
        self.cycles_completed += 1;
        self.last_cycle = stats.clone();
        self.last_error = None;
        self.last_cycle_unix = now_unix();
        self.totals.archives_extracted += stats.archives_extracted;
        self.totals.files_parsed += stats.files_parsed;
        self.totals.files_skipped += stats.files_skipped;
        self.totals.records_parsed += stats.records_parsed;
        self.totals.records_inserted += stats.records_inserted;
        self.totals.issues_found += stats.issues_found;
        self.totals.notifications_queued += stats.notifications_queued;
    }
}

fn now_unix() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

#[derive(Default)]
struct DedupState {
    parsed_hashes: HashSet<FileHash>,
    seen_paths: HashMap<PathBuf, HashSet<FileHash>>,
}

struct ArchiveAgg {
    name: String,
    output_dir: PathBuf,
    files_extracted: usize,
    files_parsed: usize,
    records_inserted: usize,
    issues_found: usize,
}

pub struct IngestService {
    repo: Arc<dyn CredentialRepository>,
    input_dir: PathBuf,
    archive_dir: PathBuf,
    status: SharedImportStatus,
    events: EventBus,
    dedup: Mutex<Option<DedupState>>,
}

impl IngestService {
    pub fn new(
        repo: Arc<dyn CredentialRepository>,
        input_dir: impl Into<PathBuf>,
        archive_dir: impl Into<PathBuf>,
        status: SharedImportStatus,
        events: EventBus,
    ) -> Self {
        Self {
            repo,
            input_dir: input_dir.into(),
            archive_dir: archive_dir.into(),
            status,
            events,
            dedup: Mutex::new(None),
        }
    }

    pub async fn run_cycle(&self) -> Result<ImportCycleStats> {
        let stats = self.cycle().await;
        match &stats {
            Ok(stats) => {
                let mut status = self.status.lock().await;
                status.record(stats);
                status.pending_archives = self.pending_archive_count().await;
            }
            Err(error) => {
                let mut status = self.status.lock().await;
                status.last_error = Some(error.to_string());
                drop(status);
                self.events.emit(AppEvent::problem(error.to_string()));
            }
        }
        stats
    }

    async fn pending_archive_count(&self) -> usize {
        let mut reader = match fs::read_dir(&self.archive_dir).await {
            Ok(reader) => reader,
            Err(_) => return 0,
        };
        let mut count = 0usize;
        while let Ok(Some(entry)) = reader.next_entry().await {
            if is_archive_file(&entry.path()) {
                count += 1;
            }
        }
        count
    }

    async fn cycle(&self) -> Result<ImportCycleStats> {
        let mut stats = ImportCycleStats::default();

        recover_orphaned_upload_requests(&self.archive_dir, &self.input_dir).await?;

        let extracted =
            process_pending_archives(&self.archive_dir, &self.input_dir, &self.events).await?;
        stats.archives_extracted = extracted.len();
        let extracted_paths: Vec<PathBuf> =
            extracted.iter().map(|e| e.output_dir.clone()).collect();
        let mut arch_agg: Vec<ArchiveAgg> = extracted
            .into_iter()
            .map(|e| ArchiveAgg {
                name: e.name,
                output_dir: e.output_dir,
                files_extracted: e.files_extracted,
                files_parsed: 0,
                records_inserted: 0,
                issues_found: 0,
            })
            .collect();

        let tracked_notifications = load_pending_notifications(&self.archive_dir)
            .await?
            .into_iter()
            .filter_map(|(notification_path, notification)| {
                notification
                    .output_dir
                    .clone()
                    .map(|output_dir| (notification_path, notification, output_dir))
            })
            .collect::<Vec<_>>();
        let mut tracked_stats = tracked_notifications
            .iter()
            .map(|(_, _, output_dir)| (output_dir.clone(), ArchiveParseSummary::default()))
            .collect::<HashMap<_, _>>();

        let mut dedup_guard = self.dedup.lock().await;
        if dedup_guard.is_none() {
            *dedup_guard = Some(DedupState {
                parsed_hashes: self.repo.load_parsed_hashes().await?,
                seen_paths: self.repo.load_seen_paths().await?,
            });
        }
        let dedup = dedup_guard.as_mut().expect("dedup initialized above");

        let input_dir = self.input_dir.clone();
        let files =
            tokio::task::spawn_blocking(move || iter_files(&input_dir).collect::<Vec<PathBuf>>())
                .await?;

        let mut cred_rows: Vec<AccountRecord> = Vec::new();
        let mut source_file_rows: Vec<SourceFileRecord> = Vec::new();
        let mut source_path_rows: Vec<SourceFilePathRecord> = Vec::new();

        for path in files {
            let tracked_output_dir = tracked_notifications.iter().find_map(|(_, _, output_dir)| {
                path.starts_with(output_dir).then_some(output_dir.clone())
            });
            let current_file_hash = match file_hash(&path).await {
                Ok(hash) => hash,
                Err(error) => {
                    warn!(error = %error, path = %path.display(), "failed to hash file");
                    continue;
                }
            };

            let file_size = fs::metadata(&path).await.map(|meta| meta.len()).unwrap_or(0);
            let same_path_same_hash = dedup
                .seen_paths
                .get(&path)
                .is_some_and(|hashes| hashes.contains(&current_file_hash));
            let parsed_before = dedup.parsed_hashes.contains(&current_file_hash);

            if same_path_same_hash {
                stats.files_skipped += 1;
                if let Some(output_dir) = tracked_output_dir.as_ref()
                    && let Some(summary) = tracked_stats.get_mut(output_dir)
                {
                    summary.files_skipped += 1;
                }
                continue;
            }

            if parsed_before {
                source_path_rows.push(SourceFilePathRecord {
                    file_hash: current_file_hash.0.clone(),
                    path: path.clone(),
                    modified_at: None,
                    file_size,
                });

                dedup
                    .seen_paths
                    .entry(path.clone())
                    .or_default()
                    .insert(current_file_hash);
                stats.files_skipped += 1;
                if let Some(output_dir) = tracked_output_dir.as_ref()
                    && let Some(summary) = tracked_stats.get_mut(output_dir)
                {
                    summary.files_skipped += 1;
                }
                continue;
            }

            let report = parse_file(&path);
            let issues_found = report.issues.len();
            let records_parsed = report.records.len();
            let records_inserted = report.records.iter().filter(|r| is_insertable(r)).count();
            stats.files_parsed += 1;
            stats.records_parsed += records_parsed;
            stats.issues_found += issues_found;
            stats.records_inserted += records_inserted;

            cred_rows.extend(report.records);
            source_file_rows.push(SourceFileRecord {
                file_hash: current_file_hash.0.clone(),
                file_size,
                parse_status: "parsed".to_string(),
                error_message: None,
            });
            source_path_rows.push(SourceFilePathRecord {
                file_hash: current_file_hash.0.clone(),
                path: path.clone(),
                modified_at: None,
                file_size,
            });

            dedup.parsed_hashes.insert(current_file_hash.clone());
            dedup
                .seen_paths
                .entry(path.clone())
                .or_default()
                .insert(current_file_hash);

            if let Some(output_dir) = tracked_output_dir.as_ref()
                && let Some(summary) = tracked_stats.get_mut(output_dir)
            {
                summary.files_parsed += 1;
                summary.records_inserted += records_inserted;
                summary.issues_found += issues_found;
            }

            if let Some(agg) = arch_agg
                .iter_mut()
                .find(|a| path.starts_with(&a.output_dir))
            {
                agg.files_parsed += 1;
                agg.records_inserted += records_inserted;
                agg.issues_found += issues_found;
            }
        }

        if let Err(error) = self.flush(&cred_rows, &source_file_rows, &source_path_rows).await {
            *dedup_guard = None;
            return Err(error);
        }
        drop(dedup_guard);

        for agg in &arch_agg {
            self.events.emit(AppEvent::archive_done(
                agg.name.clone(),
                agg.files_extracted,
                agg.files_parsed,
                agg.records_inserted,
                agg.issues_found,
            ));
        }
        if stats.did_work() {
            self.events.emit(AppEvent::import(&stats));
        }

        let mut dirs_to_remove = extracted_paths;
        for (notification_path, mut notification, output_dir) in tracked_notifications {
            let summary = tracked_stats.remove(&output_dir).unwrap_or_default();
            notification.mark_ready(summary);
            save_pending_notification(&notification_path, &notification).await?;
            stats.notifications_queued += 1;
            dirs_to_remove.push(output_dir);
        }

        let mut unique_dirs = HashSet::new();
        for path in dirs_to_remove {
            if !unique_dirs.insert(path.clone()) {
                continue;
            }

            if let Err(e) = tokio::fs::remove_dir_all(&path).await
                && e.kind() != std::io::ErrorKind::NotFound
            {
                error!(error = %e, output_dir = %path.display(), "cannot remove extracted dir");
            }
        }

        Ok(stats)
    }

    async fn flush(
        &self,
        cred_rows: &[AccountRecord],
        source_file_rows: &[SourceFileRecord],
        source_path_rows: &[SourceFilePathRecord],
    ) -> Result<()> {
        self.repo.insert_records(cred_rows).await?;
        self.repo.record_source_files(source_file_rows).await?;
        self.repo.record_source_file_paths(source_path_rows).await?;
        Ok(())
    }
}

fn is_insertable(rec: &AccountRecord) -> bool {
    rec.url().map(|u| !u.trim().is_empty()).unwrap_or(false)
}

async fn read_archive_dir(archive_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut reader = match fs::read_dir(archive_dir).await {
        Ok(reader) => reader,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };

    let mut archives = Vec::new();
    while let Some(entry) = reader.next_entry().await? {
        let path = entry.path();
        if is_archive_file(&path) {
            archives.push(path);
        }
    }
    Ok(archives)
}

struct ExtractedArchive {
    name: String,
    output_dir: PathBuf,
    files_extracted: usize,
}

fn archive_name_of(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("archive")
        .to_string()
}

async fn process_pending_archives(
    archive_dir: &Path,
    input_dir: &Path,
    events: &EventBus,
) -> Result<Vec<ExtractedArchive>> {
    let mut archives: Vec<PathBuf> = read_archive_dir(archive_dir).await?;
    archives.sort();

    let mut extracted = vec![];

    for archive_path in archives {
        let archive_name = archive_name_of(&archive_path);
        let needs_password_path = archive_needs_password_path(&archive_path);
        let pass_path = archive_password_path(&archive_path);

        let password_str: Option<String> = if pass_path.exists() {
            match fs::read_to_string(&pass_path).await {
                Ok(s) => Some(s.trim().to_string()),
                Err(e) => {
                    warn!(error = %e, pass_path = %pass_path.display(), "failed to read password file");
                    None
                }
            }
        } else {
            None
        };

        if needs_password_path.exists() && password_str.is_none() {
            debug!(archive_path = %archive_path.display(), "skipping password-protected archive awaiting password");
            continue;
        }

        info!(
            archive_path = %archive_path.display(),
            has_password = password_str.is_some(),
            "extracting archive"
        );
        events.emit(AppEvent::extracting(archive_name.clone()));

        let archive_path_for_task = archive_path.clone();
        let output_root = input_dir.to_path_buf();
        let password_for_task = password_str.clone();
        let extract_result = tokio::task::spawn_blocking(move || {
            extract_archive(
                &archive_path_for_task,
                &output_root,
                password_for_task.as_deref(),
            )
        })
        .await;

        match extract_result {
            Ok(Ok(stats)) => {
                fs::remove_file(&archive_path).await?;
                let _ = fs::remove_file(&pass_path).await;
                if let Err(e) = remove_needs_password_marker(&archive_path).await {
                    warn!(error = %e, "failed to remove needs-password marker");
                }
                if let Err(error) =
                    promote_upload_request_to_pending(archive_dir, &archive_path, &stats.output_dir)
                        .await
                {
                    warn!(
                        error = %error,
                        archive_path = %archive_path.display(),
                        output_dir = %stats.output_dir.display(),
                        "failed to promote telegram archive notification"
                    );
                }
                extracted.push(ExtractedArchive {
                    name: archive_name.clone(),
                    output_dir: stats.output_dir.clone(),
                    files_extracted: stats.files_extracted,
                });
                info!(
                    archive_path = %archive_path.display(),
                    output_dir = %stats.output_dir.display(),
                    files_extracted = stats.files_extracted,
                    "archive extracted and deleted"
                );
            }
            Ok(Err(ExtractError::PasswordRequired)) => {
                info!(
                    archive_path = %archive_path.display(),
                    "archive requires a password; waiting for password file"
                );
                if !needs_password_path.exists() {
                    let request = load_upload_request(&archive_path)
                        .await
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| ArchiveUploadRequest::local(&archive_name));
                    if let Err(e) =
                        write_needs_password_marker(&archive_path, &archive_name, request).await
                    {
                        warn!(error = %e, "failed to write needs-password marker");
                    }
                    events.emit(AppEvent::needs_password(archive_name.clone()));
                }
            }
            Ok(Err(error)) => {
                if password_str.is_some() {
                    warn!(
                        error = %error,
                        archive_path = %archive_path.display(),
                        "archive extraction failed with the supplied password; discarding it and \
                         waiting for a new one"
                    );
                    if let Err(e) = fs::remove_file(&pass_path).await {
                        warn!(error = %e, pass_path = %pass_path.display(), "failed to remove rejected password file");
                    }
                    let request = load_upload_request(&archive_path)
                        .await
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| ArchiveUploadRequest::local(&archive_name));
                    if let Err(e) =
                        write_needs_password_marker(&archive_path, &archive_name, request).await
                    {
                        warn!(error = %e, "failed to refresh needs-password marker");
                    }
                    events.emit(AppEvent::needs_password(archive_name.clone()));
                } else {
                    warn!(
                        error = %error,
                        archive_path = %archive_path.display(),
                        "archive extraction failed; will retry later"
                    );
                    events.emit(AppEvent::failed(archive_name.clone(), error.to_string()));
                }
            }
            Err(error) => {
                warn!(
                    error = %error,
                    archive_path = %archive_path.display(),
                    "archive extraction task panicked; will retry later"
                );
                events.emit(AppEvent::failed(archive_name.clone(), error.to_string()));
            }
        }
    }

    Ok(extracted)
}

async fn promote_upload_request_to_pending(
    archive_dir: &Path,
    archive_path: &Path,
    output_dir: &Path,
) -> Result<()> {
    let request = match load_upload_request(archive_path).await? {
        Some(request) => request,
        None => return Ok(()),
    };

    queue_pending_parse_notification(archive_dir, request, output_dir.to_path_buf()).await?;
    remove_upload_request(archive_path).await?;
    Ok(())
}

async fn recover_orphaned_upload_requests(archive_dir: &Path, input_dir: &Path) -> Result<()> {
    let mut reader = match fs::read_dir(archive_dir).await {
        Ok(reader) => reader,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };

    while let Some(entry) = reader.next_entry().await? {
        let request_path = entry.path();
        if !request_path.is_file() {
            continue;
        }

        let Some(archive_path) = archive_path_from_upload_request(&request_path) else {
            continue;
        };

        if archive_path.exists() {
            continue;
        }

        let output_dir = archive_output_dir(input_dir, &archive_path);
        if !output_dir.exists() {
            continue;
        }

        let request = match load_upload_request_file(&request_path).await? {
            Some(request) => request,
            None => continue,
        };

        queue_pending_parse_notification(archive_dir, request, output_dir.clone()).await?;
        fs::remove_file(&request_path).await?;
        info!(
            request_path = %request_path.display(),
            output_dir = %output_dir.display(),
            "recovered orphaned telegram archive notification"
        );
    }

    Ok(())
}
