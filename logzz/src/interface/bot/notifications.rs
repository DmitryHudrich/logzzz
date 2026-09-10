use eyre::Result;
use std::path::Path;
use teloxide::prelude::*;
use teloxide::types::ChatId;
use tokio::fs;
use tracing::warn;

use crate::infrastructure::telegram_ipc::{
    format_ready_notification, load_pending_notifications, save_needs_password_marker,
    scan_needs_password_archives,
};

pub async fn flush_password_request_notifications(bot: &Bot, archive_dir: &Path) -> Result<usize> {
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

pub async fn flush_ready_notifications(bot: &Bot, archive_dir: &Path) -> Result<usize> {
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
