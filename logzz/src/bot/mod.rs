mod archive;
mod handlers;
mod html;
mod query;
mod state;
mod types;

pub use state::BotState;

use eyre::Result;
use std::sync::Arc;
use teloxide::prelude::*;

use archive::handle_document;
use handlers::{Command, handle_callback, handle_command};

pub async fn start_bot(state: BotState, bot: Bot) -> Result<()> {
    let state = Arc::new(state);

    let handler = dptree::entry()
        .branch(
            Update::filter_message()
                .filter_command::<Command>()
                .endpoint(handle_command),
        )
        .branch(
            Update::filter_message()
                .filter(|msg: Message| msg.document().is_some())
                .endpoint(handle_document),
        )
        .branch(Update::filter_callback_query().endpoint(handle_callback));

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![state])
        .build()
        .dispatch()
        .await;

    Ok(())
}
