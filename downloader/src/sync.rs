use grammers_client::media::Media;
use grammers_client::Client;
use logzz::archive::{
    archive_password_path, build_archive_filename, detect_archive_kind, find_archive_by_message_id,
    partial_archive_path,
};
use logzz::telegram::{
    ArchiveUploadRequest, format_ready_notification, load_pending_notifications,
    remove_upload_request, save_needs_password_marker, scan_needs_password_archives,
    write_upload_request,
};
use std::io;
use std::path::Path;
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};

use crate::state::{DownloaderState, save_state};
use crate::Result;

pub fn is_archive_name(name: &str) -> bool {
    detect_archive_kind(Path::new(name)).is_some()
}

pub async fn resolve_peer(
    client: &Client,
    peer_name: &str,
) -> Result<grammers_session::types::PeerRef> {
    let user = client
        .resolve_username(peer_name)
        .await?
        .ok_or_else(|| format!("no peer with username `{peer_name}`"))?;

    user.to_ref()
        .await
        .ok_or_else(|| format!("peer `{peer_name}` could not be resolved to a reference").into())
}

pub async fn sync_new_archives(
    client: &Client,
    peer: grammers_session::types::PeerRef,
    peer_name: &str,
    archive_dir: &Path,
    state_path: &Path,
    state: &mut DownloaderState,
) -> Result<usize> {
    let mut messages = client.iter_messages(peer);
    let mut archive_message_ids = Vec::new();
    let mut password_replies: Vec<(i32, String)> = Vec::new();

    while let Some(msg) = messages.next().await? {
        if msg.id() <= state.last_downloaded_archive_message_id {
            break;
        }

        if let Some(Media::Document(document)) = msg.media() {
            let file_name = document.name().unwrap_or_default();
            if is_archive_name(file_name) {
                archive_message_ids.push(msg.id());
            }
        } else {
            let text = msg.text();
            if !text.is_empty()
                && let Some(reply_to_id) = msg.reply_to_message_id() {
                    password_replies.push((reply_to_id, text.trim().to_string()));
                }
        }
    }

    // Save passwords from replies before downloading new archives
    for (reply_to_id, password_text) in password_replies {
        if let Some(archive_path) = find_archive_by_message_id(archive_dir, reply_to_id) {
            let pass_path = archive_password_path(&archive_path);
            if let Err(e) = tokio::fs::write(&pass_path, password_text.as_bytes()).await {
                warn!(
                    error = %e,
                    reply_to_id,
                    pass_path = %pass_path.display(),
                    "failed to save archive password from reply"
                );
            } else {
                info!(
                    reply_to_id,
                    archive_path = %archive_path.display(),
                    "saved archive password from reply"
                );
            }
        }
    }

    if archive_message_ids.is_empty() {
        return Ok(0);
    }

    archive_message_ids.sort_unstable();

    let mut downloaded = 0usize;

    for message_id in archive_message_ids {
        let fetched = client.get_messages_by_id(peer, &[message_id]).await?;
        let Some(message) = fetched.into_iter().next().flatten() else {
            warn!(message_id, "archive message disappeared before download");
            state.last_downloaded_archive_message_id = message_id;
            save_state(state_path, state).await?;
            continue;
        };

        let Some(Media::Document(document)) = message.media() else {
            warn!(message_id, "message no longer contains a document");
            state.last_downloaded_archive_message_id = message_id;
            save_state(state_path, state).await?;
            continue;
        };

        let original_name = document.name().unwrap_or("archive");
        if !is_archive_name(original_name) {
            state.last_downloaded_archive_message_id = message_id;
            save_state(state_path, state).await?;
            continue;
        }

        let Some(archive_kind) = detect_archive_kind(Path::new(original_name)) else {
            // is_archive_name() above already confirmed this succeeds; unreachable in practice.
            state.last_downloaded_archive_message_id = message_id;
            save_state(state_path, state).await?;
            continue;
        };
        let final_path = archive_dir.join(build_archive_filename(
            message_id,
            Some(original_name),
            archive_kind,
        ));
        let temp_path = partial_archive_path(&final_path);

        info!(
            message_id,
            archive_path = %final_path.display(),
            original_name,
            "downloading archive"
        );

        let mut download = client.iter_download(&document);

        let Some(peer1) = message.peer_ref().await else {
            warn!(message_id, "failed to resolve message peer reference; skipping");
            continue;
        };
        let msg = client.send_message(peer1, "Download started").await?;
        let upload_request =
            ArchiveUploadRequest::for_userbot(peer_name, message_id, msg.id(), original_name);
        write_upload_request(&final_path, &upload_request).await?;

        let mut file = tokio::fs::File::create(&temp_path).await?;
        let mut downloaded_bytes = 0usize;
        let total = document.size();

        let mut iteration = 0usize;

        while let Some(chunk) = download.next().await.map_err(io::Error::other)? {
            file.write_all(&chunk).await?;
            downloaded_bytes += chunk.len();
            iteration += 1;

            if iteration.is_multiple_of(5)
                && let Some(total) = total
            {
                let progress = downloaded_bytes as f64 / total as f64 * 100.0;
                client
                    .edit_message(peer1, msg.id(), format!("Downloaded {progress:.2}%"))
                    .await?;
            }
        }

        if let Err(error) = tokio::fs::rename(&temp_path, &final_path).await {
            let _ = remove_upload_request(&final_path).await;
            return Err(Box::new(error));
        }

        client
            .edit_message(
                peer1,
                msg.id(),
                format!("Downloaded {original_name}. Waiting for parsing."),
            )
            .await?;

        state.last_downloaded_archive_message_id = message_id;
        save_state(state_path, state).await?;
        downloaded += 1;
    }

    Ok(downloaded)
}

pub async fn flush_needs_password_notifications(
    client: &Client,
    peer: grammers_session::types::PeerRef,
    peer_name: &str,
    archive_dir: &Path,
) -> Result<usize> {
    let mut sent = 0usize;

    for (archive_path, mut marker) in scan_needs_password_archives(archive_dir).await? {
        if marker.notification_sent {
            continue;
        }

        let Some(_) = marker.request.userbot_progress_message_id(peer_name) else {
            continue;
        };

        let message = format!(
            "Archive '{}' requires a password to extract.\n\
             Reply to the forwarded archive message with the password.",
            marker.archive_name
        );

        match client.send_message(peer, message).await {
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
                    peer_name,
                    "failed to deliver needs-password notification to userbot peer"
                );
            }
        }
    }

    Ok(sent)
}

pub async fn flush_parse_notifications(
    client: &Client,
    peer: grammers_session::types::PeerRef,
    peer_name: &str,
    archive_dir: &Path,
) -> Result<usize> {
    let mut updated = 0usize;

    for (notification_path, notification) in load_pending_notifications(archive_dir).await? {
        if !notification.is_ready() {
            continue;
        }

        let Some(progress_message_id) = notification.request.userbot_progress_message_id(peer_name)
        else {
            continue;
        };

        let Some(message) = format_ready_notification(&notification) else {
            continue;
        };

        match client
            .edit_message(peer, progress_message_id, message.clone())
            .await
        {
            Ok(_) => {
                tokio::fs::remove_file(&notification_path).await?;
                updated += 1;
            }
            Err(edit_error) => {
                warn!(
                    error = %edit_error,
                    progress_message_id,
                    notification_path = %notification_path.display(),
                    "failed to edit progress message; sending a new one"
                );

                match client.send_message(peer, &message).await {
                    Ok(_) => {
                        tokio::fs::remove_file(&notification_path).await?;
                        updated += 1;
                    }
                    Err(send_error) => {
                        warn!(
                            error = %send_error,
                            progress_message_id,
                            notification_path = %notification_path.display(),
                            "failed to send parse notification to userbot peer"
                        );
                    }
                }
            }
        }
    }

    Ok(updated)
}
