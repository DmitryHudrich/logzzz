use eyre::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use teloxide::net::Download;
use teloxide::prelude::*;
use teloxide::types::{Document, FileId, Message};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::{error, info, warn};

use crate::archive::{build_archive_filename, detect_archive_kind, partial_archive_path};
use crate::telegram::{ArchiveUploadRequest, remove_upload_request, write_upload_request};

use super::state::BotState;

pub async fn handle_document(bot: Bot, msg: Message, state: Arc<BotState>) -> ResponseResult<()> {
    let doc: &Document = match msg.document() {
        Some(document) => document,
        None => return Ok(()),
    };

    let original_name = doc.file_name.as_deref().unwrap_or("archive");
    let archive_kind = match detect_archive_kind(Path::new(original_name)) {
        Some(kind) => kind,
        None => return Ok(()),
    };

    if let Err(error) = fs::create_dir_all(&state.archive_dir).await {
        error!(error = %error, archive_dir = %state.archive_dir, "failed to create archive inbox");
        bot.send_message(
            msg.chat.id,
            format!("Archive inbox is unavailable: {error}"),
        )
        .await?;
        return Ok(());
    }

    let archive_name = build_archive_filename(msg.id.0, Some(original_name), archive_kind);
    let final_path = PathBuf::from(&state.archive_dir).join(archive_name);
    let temp_path = partial_archive_path(&final_path);
    let upload_request = ArchiveUploadRequest::for_bot(msg.chat.id.0, msg.id.0, original_name);

    info!(
        chat_id = msg.chat.id.0,
        message_id = msg.id.0,
        archive_path = %final_path.display(),
        "queueing archive from telegram bot"
    );

    bot.send_message(
        msg.chat.id,
        format!(
            "Archive accepted: `{}`\nQueued for extraction and parsing.",
            original_name
        ),
    )
    .await?;

    if let Err(error) = download_file(&bot, &doc.file.id.0, &temp_path).await {
        error!(
            error = %error,
            temp_path = %temp_path.display(),
            "archive download failed"
        );
        let _ = fs::remove_file(&temp_path).await;
        bot.send_message(msg.chat.id, format!("Archive download failed: {error}"))
            .await?;
        return Ok(());
    }

    let notification_enabled = match write_upload_request(&final_path, &upload_request).await {
        Ok(()) => true,
        Err(error) => {
            warn!(
                error = %error,
                archive_path = %final_path.display(),
                "failed to stage telegram archive notification"
            );
            false
        }
    };

    if let Err(error) = fs::rename(&temp_path, &final_path).await {
        error!(
            error = %error,
            temp_path = %temp_path.display(),
            final_path = %final_path.display(),
            "failed to finalize downloaded archive"
        );
        let _ = fs::remove_file(&temp_path).await;
        if notification_enabled {
            let _ = remove_upload_request(&final_path).await;
        }
        bot.send_message(
            msg.chat.id,
            format!("Archive download completed but finalize failed: {error}"),
        )
        .await?;
        return Ok(());
    }

    info!(
        chat_id = msg.chat.id.0,
        message_id = msg.id.0,
        archive_kind = ?archive_kind,
        archive_path = %final_path.display(),
        "archive queued successfully"
    );

    bot.send_message(
        msg.chat.id,
        format!(
            "Archive queued: `{}`\nInbox: `{}`{}",
            original_name,
            state.archive_dir,
            if notification_enabled {
                "\nYou'll receive a message when parsing finishes."
            } else {
                ""
            }
        ),
    )
    .await?;

    Ok(())
}

async fn download_file(bot: &Bot, file_id: &str, dest: &Path) -> Result<()> {
    let tg_file = bot.get_file(FileId(file_id.to_string())).await?;
    let mut out = fs::File::create(dest).await?;
    bot.download_file(&tg_file.path, &mut out).await?;
    out.flush().await?;
    Ok(())
}
