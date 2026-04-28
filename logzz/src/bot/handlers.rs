use std::sync::Arc;

use teloxide::{
    prelude::*,
    types::{InlineKeyboardButton, InlineKeyboardMarkup, InputFile, Message, ParseMode},
    utils::command::BotCommands,
};
use tokio::fs;

use super::html::{render_html_report, sanitize_filename};
use super::query::{build_record, fetch_all_paths, fetch_grouped, fetch_total_count};
use super::state::{BotState, FETCH_LIMIT, PAGE_SIZE, Session};
use super::types::CredRecord;

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
        Command::Url(query) => {
            let query = query.trim().to_string();
            if query.is_empty() {
                bot.send_message(msg.chat.id, "⚠️ Usage: `/url example.com`")
                    .parse_mode(ParseMode::MarkdownV2)
                    .await?;
                return Ok(());
            }
            start_search(&bot, &msg, &state, query, "url").await?;
        }
        Command::Login(query) => {
            let query = query.trim().to_string();
            if query.is_empty() {
                bot.send_message(msg.chat.id, "⚠️ Usage: `/login user@example.com`")
                    .parse_mode(ParseMode::MarkdownV2)
                    .await?;
                return Ok(());
            }
            start_search(&bot, &msg, &state, query, "login").await?;
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
    search_type: &str,
) -> ResponseResult<()> {
    let search_id: u32 = rand::random();
    let chat_id = msg.chat.id.0;

    bot.send_message(
        msg.chat.id,
        format!(
            "🔍 Searching by {}: `{}`…",
            search_type.to_uppercase(),
            query
        ),
    )
    .parse_mode(ParseMode::MarkdownV2)
    .await?;

    state.sessions.insert(
        (chat_id, search_id),
        Session {
            query: query.clone(),
            search_type: search_type.to_string(),
            page: 0,
            has_next: false,
        },
    );

    deliver_page(bot, msg.chat.id, state, search_id, &query, search_type, 0).await?;
    Ok(())
}

async fn deliver_page(
    bot: &Bot,
    chat: ChatId,
    state: &Arc<BotState>,
    search_id: u32,
    query: &str,
    search_type: &str,
    page: usize,
) -> ResponseResult<()> {
    let offset = page * PAGE_SIZE;

    let raw = match fetch_grouped(&state.client, query, search_type, FETCH_LIMIT + 1, offset).await
    {
        Ok(r) => r,
        Err(e) => {
            bot.send_message(chat, format!("❌ Query error: {e}"))
                .await?;
            return Ok(());
        }
    };

    if raw.is_empty() && page == 0 {
        bot.send_message(chat, format!("🔎 No results found for `{}`", query))
            .parse_mode(ParseMode::MarkdownV2)
            .await?;
        return Ok(());
    }

    if raw.is_empty() {
        bot.send_message(chat, "📭 No more results.").await?;
        return Ok(());
    }

    let has_next = raw.len() > FETCH_LIMIT;
    let raw_page = if has_next {
        &raw[..FETCH_LIMIT]
    } else {
        &raw[..]
    };
    let all_hashes: Vec<String> = {
        let mut set = std::collections::HashSet::new();
        for row in raw_page {
            for h in &row.file_hashes {
                set.insert(h.clone());
            }
        }
        set.into_iter().collect()
    };

    let hash_to_paths = fetch_all_paths(&state.client, &all_hashes)
        .await
        .unwrap_or_default();
    let records: Vec<CredRecord> = raw_page
        .iter()
        .map(|r| build_record(r, &hash_to_paths))
        .collect();

    state.sessions.insert(
        (chat.0, search_id),
        Session {
            query: query.to_string(),
            search_type: search_type.to_string(),
            page,
            has_next,
        },
    );

    let total_unique = fetch_total_count(&state.client, query, search_type)
        .await
        .unwrap_or(0);
    let total_paths: usize = records.iter().map(|r| r.all_paths.len()).sum();

    let html = render_html_report(&records, query, search_type, page, has_next, total_unique);
    let filename = sanitize_filename(query, search_type, page);
    let filepath = format!("{}/{}", state.results_dir, filename);

    fs::create_dir_all(&state.results_dir).await.ok();

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
