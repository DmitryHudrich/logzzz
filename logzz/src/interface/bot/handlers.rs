use std::sync::Arc;

use teloxide::{
    prelude::*,
    types::{InlineKeyboardButton, InlineKeyboardMarkup, InputFile, Message, ParseMode},
    utils::command::BotCommands,
};
use tokio::fs;

use super::html::{render_html_report, sanitize_filename};
use super::state::{BotState, PAGE_SIZE, Session};
use crate::domain::repository::SearchType;

const REPORT_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

async fn cleanup_old_reports(results_dir: &str) {
    let mut reader = match fs::read_dir(results_dir).await {
        Ok(reader) => reader,
        Err(_) => return,
    };

    while let Ok(Some(entry)) = reader.next_entry().await {
        let path = entry.path();
        let is_report = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("logzz_") && n.ends_with(".html"));
        if !is_report {
            continue;
        }

        let too_old = entry
            .metadata()
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age > REPORT_TTL);

        if too_old {
            let _ = fs::remove_file(&path).await;
        }
    }
}

fn split_query_and_tags(input: &str) -> (String, Vec<String>) {
    let mut terms = Vec::new();
    let mut tags = Vec::new();

    for token in input.split_whitespace() {
        if let Some(tag) = token.strip_prefix('#') {
            let tag = tag.trim();
            if !tag.is_empty() {
                tags.push(tag.to_string());
            }
        } else {
            terms.push(token);
        }
    }

    (terms.join(" "), tags)
}

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase", description = "Logzz Search Bot")]
pub enum Command {
    #[command(description = "Show help")]
    Help,
    #[command(description = "Search by URL or domain: /url <query>")]
    Url(String),
    #[command(description = "Search by login/username: /login <query>")]
    Login(String),
}

pub async fn handle_command(
    bot: Bot,
    msg: Message,
    cmd: Command,
    state: Arc<BotState>,
) -> ResponseResult<()> {
    match cmd {
        Command::Help => {
            bot.send_message(
                msg.chat.id,
                "🔍 *Logzz Search Bot*\n\n\
                 `/url <domain>` — search by URL / domain\n\
                 `/login <username>` — search by username / email\n\n\
                 *Upload archives:* send or forward a `\\.zip` or `\\.rar` file — \
                 the bot will queue it for background extraction and parsing.\n\n\
                 Results arrive as HTML reports\\. Use ◀ ▶ to page through all matches\\.\n\
                 Each page contains up to 50 unique credentials\\.",
            )
            .parse_mode(ParseMode::MarkdownV2)
            .await?;
        }
        Command::Url(raw) => {
            let (query, tags) = split_query_and_tags(&raw);
            if query.is_empty() && tags.is_empty() {
                bot.send_message(msg.chat.id, "⚠️ Usage: `/url example.com` or `/url #tag`")
                    .parse_mode(ParseMode::MarkdownV2)
                    .await?;
                return Ok(());
            }
            start_search(&bot, &msg, &state, query, tags, "url").await?;
        }
        Command::Login(raw) => {
            let (query, tags) = split_query_and_tags(&raw);
            if query.is_empty() && tags.is_empty() {
                bot.send_message(msg.chat.id, "⚠️ Usage: `/login user@example.com` or `/login #tag`")
                    .parse_mode(ParseMode::MarkdownV2)
                    .await?;
                return Ok(());
            }
            start_search(&bot, &msg, &state, query, tags, "login").await?;
        }
    }

    Ok(())
}

pub async fn handle_callback(
    bot: Bot,
    q: CallbackQuery,
    state: Arc<BotState>,
) -> ResponseResult<()> {
    bot.answer_callback_query(q.id).await?;

    let data = match q.data.as_deref() {
        Some(d) => d,
        None => return Ok(()),
    };

    let parts: Vec<&str> = data.splitn(4, ':').collect();
    if parts.len() != 4 || parts[0] != "page" {
        return Ok(());
    }

    let chat_id: i64 = match parts[1].parse() {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    let search_id: u32 = match parts[2].parse() {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    let direction = parts[3];
    let chat = ChatId(chat_id);

    let session = match state.sessions.get(&(chat_id, search_id)) {
        Some(s) => s.clone(),
        None => {
            bot.send_message(chat, "⚠️ Session expired. Please run the search again.")
                .await?;
            return Ok(());
        }
    };

    let new_page = match direction {
        "next" if session.has_next => session.page + 1,
        "prev" if session.page > 0 => session.page - 1,
        _ => return Ok(()),
    };

    deliver_page(
        &bot,
        chat,
        &state,
        search_id,
        &session.query,
        &session.search_type,
        &session.tags,
        new_page,
    )
    .await?;

    Ok(())
}

async fn start_search(
    bot: &Bot,
    msg: &Message,
    state: &Arc<BotState>,
    query: String,
    tags: Vec<String>,
    search_type: &str,
) -> ResponseResult<()> {
    state.prune_sessions();

    let search_id: u32 = rand::random();
    let chat_id = msg.chat.id.0;

    let tag_note = if tags.is_empty() {
        String::new()
    } else {
        format!(" (tags: {})", tags.join(", "))
    };

    bot.send_message(
        msg.chat.id,
        format!(
            "🔍 Searching by {}: `{}`{}…",
            search_type.to_uppercase(),
            query,
            tag_note,
        ),
    )
    .parse_mode(ParseMode::MarkdownV2)
    .await?;

    state.sessions.insert(
        (chat_id, search_id),
        Session {
            query: query.clone(),
            search_type: search_type.to_string(),
            tags: tags.clone(),
            page: 0,
            has_next: false,
            updated: std::time::Instant::now(),
        },
    );

    deliver_page(bot, msg.chat.id, state, search_id, &query, search_type, &tags, 0).await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn deliver_page(
    bot: &Bot,
    chat: ChatId,
    state: &Arc<BotState>,
    search_id: u32,
    query: &str,
    search_type: &str,
    tags: &[String],
    page: usize,
) -> ResponseResult<()> {
    let result = state
        .search
        .search(query, SearchType::from_str_lossy(search_type), tags, &[], false, page)
        .await;

    let result = match result {
        Ok(r) => r,
        Err(e) => {
            bot.send_message(chat, format!("❌ Query error: {e}"))
                .await?;
            return Ok(());
        }
    };

    if result.records.is_empty() && page == 0 {
        bot.send_message(chat, format!("🔎 No results found for `{}`", query))
            .parse_mode(ParseMode::MarkdownV2)
            .await?;
        return Ok(());
    }

    if result.records.is_empty() {
        bot.send_message(chat, "📭 No more results.").await?;
        return Ok(());
    }

    let has_next = result.has_next;
    let records = result.records;
    let total_unique = result.total_unique;

    state.sessions.insert(
        (chat.0, search_id),
        Session {
            query: query.to_string(),
            search_type: search_type.to_string(),
            tags: tags.to_vec(),
            page,
            has_next,
            updated: std::time::Instant::now(),
        },
    );

    let total_paths: usize = records.iter().map(|r| r.all_paths.len()).sum();

    let html = render_html_report(&records, query, search_type, page, has_next, total_unique);
    let filename = sanitize_filename(query, search_type, page);
    let filepath = format!("{}/{}", state.results_dir, filename);

    fs::create_dir_all(&state.results_dir).await.ok();
    cleanup_old_reports(&state.results_dir).await;

    if let Err(e) = fs::write(&filepath, &html).await {
        bot.send_message(chat, format!("❌ Failed to write report: {e}"))
            .await?;
        return Ok(());
    }

    let keyboard = build_keyboard(chat.0, search_id, page, has_next);
    let first = page * PAGE_SIZE + 1;
    let last = first + records.len() - 1;

    bot.send_message(
        chat,
        format!(
            "📄 Page *{}* · records *{}–{}* of *{}* unique\n\
             📂 *{}* total source file occurrence\\(s\\) on this page",
            page + 1,
            first,
            last,
            total_unique,
            total_paths,
        ),
    )
    .parse_mode(ParseMode::MarkdownV2)
    .await?;

    bot.send_document(chat, InputFile::file(&filepath).file_name(filename))
        .reply_markup(keyboard)
        .await?;

    Ok(())
}

fn build_keyboard(
    chat_id: i64,
    search_id: u32,
    page: usize,
    has_next: bool,
) -> InlineKeyboardMarkup {
    let mut row: Vec<InlineKeyboardButton> = Vec::new();

    if page > 0 {
        row.push(InlineKeyboardButton::callback(
            "◀ Prev",
            format!("page:{}:{}:prev", chat_id, search_id),
        ));
    }

    row.push(InlineKeyboardButton::callback(
        format!("· {} ·", page + 1),
        format!("noop:{}:{}:{}", chat_id, search_id, page),
    ));

    if has_next {
        row.push(InlineKeyboardButton::callback(
            "Next ▶",
            format!("page:{}:{}:next", chat_id, search_id),
        ));
    }

    InlineKeyboardMarkup::new(vec![row])
}

#[cfg(test)]
mod tests {
    use super::split_query_and_tags;

    #[test]
    fn splits_term_from_hash_tags() {
        let (term, tags) = split_query_and_tags("example.com #vip #checked");
        assert_eq!(term, "example.com");
        assert_eq!(tags, vec!["vip".to_string(), "checked".to_string()]);
    }

    #[test]
    fn tags_only_query_has_empty_term() {
        let (term, tags) = split_query_and_tags("  #vip ");
        assert_eq!(term, "");
        assert_eq!(tags, vec!["vip".to_string()]);
    }

    #[test]
    fn plain_query_has_no_tags() {
        let (term, tags) = split_query_and_tags("user@example.com");
        assert_eq!(term, "user@example.com");
        assert!(tags.is_empty());
    }
}
