use clickhouse::Client;
use eyre::Result;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use teloxide::prelude::*;
use teloxide::types::ChatId;
use tokio::fs;
use tracing::{debug, error, info, warn};

use crate::archive::{
    archive_needs_password_path, archive_output_dir, archive_password_path, extract_archive,
    is_archive_file, ExtractError,
};
use crate::migrate;
use crate::parser::parse_file;
use crate::telegram::{
    ArchiveParseSummary, archive_path_from_upload_request, format_ready_notification,
    load_pending_notifications, load_upload_request,
    load_upload_request_file, queue_pending_parse_notification, remove_needs_password_marker,
    remove_upload_request, save_needs_password_marker, save_pending_notification,
    scan_needs_password_archives, write_needs_password_marker,
};

use super::db::{
    SourceFilePathRow, SourceFileRow, flush_source_file_paths, flush_source_files, insert_records,
    load_parsed_hashes, load_seen_paths,
};
use super::files::{file_hash, iter_files};

#[derive(Debug, Default)]
struct ImportCycleStats {
    archives_extracted: usize,
    files_parsed: usize,
    files_skipped: usize,
    records_parsed: usize,
    records_inserted: usize,
    issues_found: usize,
    notifications_queued: usize,
}

pub async fn start(
    ch_url: &str,
    ch_user: &str,
    ch_password: &str,
    ch_database: &str,
    migrations_dir: &str,
    input_dir: &str,
    archive_dir: &str,
    poll_interval: Duration,
    telegram_bot: Option<Bot>,
) -> Result<()> {
    let bootstrap_client = Client::default()
        .with_url(ch_url)
        .with_user(ch_user)
        .with_password(ch_password)
        .with_database(ch_database);

    bootstrap_client
        .query(&format!("CREATE DATABASE IF NOT EXISTS {}", ch_database))
        .execute()
        .await?;

    let client = Client::default()
        .with_url(ch_url)
        .with_user(ch_user)
        .with_password(ch_password)
        .with_database(ch_database);

    migrate::run_migrations(&client, migrations_dir).await?;

    let input_dir = PathBuf::from(input_dir);
    let archive_dir = PathBuf::from(archive_dir);

    fs::create_dir_all(&input_dir).await?;
    fs::create_dir_all(&archive_dir).await?;

    info!(
        input_dir = %input_dir.display(),
        archive_dir = %archive_dir.display(),
        poll_interval_secs = poll_interval.as_secs(),
        "importer daemon started"
    );

    loop {
        match run_cycle(&client, &input_dir, &archive_dir).await {
            Ok(stats) => {
                if stats.archives_extracted > 0
                    || stats.files_parsed > 0
                    || stats.records_inserted > 0
                    || stats.notifications_queued > 0
                {
                    info!(
                        archives_extracted = stats.archives_extracted,
                        files_parsed = stats.files_parsed,
                        files_skipped = stats.files_skipped,
                        records_parsed = stats.records_parsed,
                        records_inserted = stats.records_inserted,
                        issues_found = stats.issues_found,
                        notifications_queued = stats.notifications_queued,
                        "import cycle completed"
                    );
                } else {
                    debug!("import cycle completed with no new work");
                }
            }
            Err(error) => {
                error!(error = %error, "import cycle failed");
            }
        }

        if let Some(bot) = telegram_bot.as_ref() {
            match flush_password_request_notifications(bot, &archive_dir).await {
                Ok(sent) if sent > 0 => {
                    info!(sent, "telegram needs-password notifications delivered");
                }
                Ok(_) => {}
                Err(error) => {
                    warn!(error = %error, "failed to flush needs-password notifications");
                }
            }

            match flush_ready_notifications(bot, &archive_dir).await {
                Ok(sent) if sent > 0 => {
                    info!(sent, "telegram archive notifications delivered");
                }
                Ok(_) => {}
                Err(error) => {
                    warn!(error = %error, "failed to flush telegram archive notifications");
                }
            }
        }

        tokio::time::sleep(poll_interval).await;
    }
}

async fn run_cycle(
    client: &Client,
    input_dir: &PathBuf,
    archive_dir: &PathBuf,
) -> Result<ImportCycleStats> {
    let mut stats = ImportCycleStats::default();

    recover_orphaned_upload_requests(archive_dir, input_dir).await?;

    let extracted_paths = process_pending_archives(archive_dir, input_dir).await?;
    stats.archives_extracted = extracted_paths.len();

    let tracked_notifications = load_pending_notifications(archive_dir)
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

    let mut parsed_hashes = load_parsed_hashes(client).await?;
    let mut seen_paths = load_seen_paths(client).await?;

    for path in iter_files(input_dir) {
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

        let file_size = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        let path_hashes = seen_paths.get(&path);
        let same_path_same_hash =
            path_hashes.is_some_and(|hashes| hashes.contains(&current_file_hash));
        let parsed_before = parsed_hashes.contains(&current_file_hash);

        if same_path_same_hash {
            stats.files_skipped += 1;
            if let Some(output_dir) = tracked_output_dir.as_ref() {
                if let Some(summary) = tracked_stats.get_mut(output_dir) {
                    summary.files_skipped += 1;
                }
            }
            continue;
        }

        if parsed_before {
            flush_source_file_paths(
                client,
                &mut vec![SourceFilePathRow {
                    file_hash: current_file_hash.0.clone(),
                    path: path.clone(),
                    modified_at: None,
                    file_size,
                }],
            )
            .await?;

            seen_paths
                .entry(path.clone())
                .or_default()
                .insert(current_file_hash);
            stats.files_skipped += 1;
            if let Some(output_dir) = tracked_output_dir.as_ref() {
                if let Some(summary) = tracked_stats.get_mut(output_dir) {
                    summary.files_skipped += 1;
                }
            }
            continue;
        }

        let report = parse_file(&path);
        let issues_found = report.issues().len();
        let records_parsed = report.records().len();
        stats.files_parsed += 1;
        stats.records_parsed += records_parsed;
        stats.issues_found += issues_found;
        let records_inserted = insert_records(client, report.records()).await?;
        stats.records_inserted += records_inserted;

        flush_source_files(
            client,
            &mut vec![SourceFileRow {
                file_hash: current_file_hash.0.clone(),
                file_size,
                parse_status: "parsed".to_string(),
                error_message: None,
            }],
        )
        .await?;

        flush_source_file_paths(
            client,
            &mut vec![SourceFilePathRow {
                file_hash: current_file_hash.0.clone(),
                path: path.clone(),
                modified_at: None,
                file_size,
            }],
        )
        .await?;

        parsed_hashes.insert(current_file_hash.clone());
        seen_paths
            .entry(path)
            .or_default()
            .insert(current_file_hash);

        if let Some(output_dir) = tracked_output_dir.as_ref() {
            if let Some(summary) = tracked_stats.get_mut(output_dir) {
                summary.files_parsed += 1;
                summary.records_inserted += records_inserted;
                summary.issues_found += issues_found;
            }
        }
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

        if let Err(e) = tokio::fs::remove_dir_all(&path).await {
            if e.kind() != std::io::ErrorKind::NotFound {
                error!(error = %e, output_dir = %path.display(), "cannot remove extracted dir");
            }
        }
    }

    Ok(stats)
}

async fn process_pending_archives(
    archive_dir: &PathBuf,
    input_dir: &PathBuf,
) -> Result<Vec<PathBuf>> {
    let mut archives: Vec<PathBuf> = std::fs::read_dir(archive_dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_archive_file(path))
        .collect();

    archives.sort();

    let mut extracted = vec![];

    for archive_path in archives {
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

        // If already waiting for a password and none has been provided yet, skip
        if needs_password_path.exists() && password_str.is_none() {
            debug!(archive_path = %archive_path.display(), "skipping password-protected archive awaiting password");
            continue;
        }

        info!(
            archive_path = %archive_path.display(),
            has_password = password_str.is_some(),
            "extracting archive"
        );

        let archive_path_for_task = archive_path.clone();
        let output_root = input_dir.clone();
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
                extracted.push(stats.output_dir.clone());
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
                // Only write marker if it doesn't exist yet (avoid resetting notification_sent)
                if !needs_password_path.exists() {
                    let original_name = archive_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("archive")
                        .to_string();
                    if let Ok(Some(request)) = load_upload_request(&archive_path).await {
                        if let Err(e) =
                            write_needs_password_marker(&archive_path, &original_name, request)
                                .await
                        {
                            warn!(error = %e, "failed to write needs-password marker");
                        }
                    }
                }
            }
            Ok(Err(error)) => {
                warn!(
                    error = %error,
                    archive_path = %archive_path.display(),
                    "archive extraction failed; will retry later"
                );
            }
            Err(error) => {
                warn!(
                    error = %error,
                    archive_path = %archive_path.display(),
                    "archive extraction task panicked; will retry later"
                );
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

async fn flush_password_request_notifications(bot: &Bot, archive_dir: &Path) -> Result<usize> {
    let mut sent = 0usize;

    for (archive_path, mut marker) in scan_needs_password_archives(archive_dir).await? {
        if marker.notification_sent {
            continue;
        }

        let Some(chat_id) = marker.request.bot_chat_id() else {
            continue;
        };

        let message = format!(
            "Archive '{}' requires a password to extract.\n\
             Reply to the original archive message with the password.",
            marker.archive_name
        );

        match bot.send_message(ChatId(chat_id), message).await {
            Ok(_) => {
                marker.notification_sent = true;
                if let Err(e) = save_needs_password_marker(&archive_path, &marker).await {
                    warn!(error = %e, "failed to update needs-password marker");
                }
                sent += 1;
            }
            Err(error) => {
                warn!(
                    error = %error,
                    archive_path = %archive_path.display(),
                    chat_id,
                    "failed to deliver needs-password notification"
                );
            }
        }
    }

    Ok(sent)
}

async fn flush_ready_notifications(bot: &Bot, archive_dir: &Path) -> Result<usize> {
    let mut sent = 0usize;

    for (notification_path, notification) in load_pending_notifications(archive_dir).await? {
        if !notification.is_ready() {
            continue;
        }

        let Some(message) = format_ready_notification(&notification) else {
            continue;
        };

        let Some(chat_id) = notification.request.bot_chat_id() else {
            continue;
        };

        match bot.send_message(ChatId(chat_id), message).await {
            Ok(_) => {
                fs::remove_file(&notification_path).await?;
                sent += 1;
            }
            Err(error) => {
                warn!(
                    error = %error,
                    notification_path = %notification_path.display(),
                    chat_id,
                    "failed to deliver telegram archive notification"
                );
            }
        }
    }

    Ok(sent)
}
