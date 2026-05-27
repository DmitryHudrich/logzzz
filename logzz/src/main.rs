use clap::Parser;
use clickhouse::Client;
use eyre::Result;
use std::sync::Arc;
use std::time::Duration;
use teloxide::Bot;
use teloxide::net::default_reqwest_settings;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use logzz::bot::{BotState, start_bot};
use logzz::config::{Cli, load_config};
use logzz::importer;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let cli = Cli::parse();
    let cfg = load_config(&cli)?;

    let client = Arc::new(
        Client::default()
            .with_url(&cfg.clickhouse.url)
            .with_user(&cfg.clickhouse.user)
            .with_password(&cfg.clickhouse.password)
            .with_database(&cfg.clickhouse.database),
    );

    let state = BotState::new(
        client,
        cfg.telegram.results_dir.clone(),
        cfg.input_dir.clone(),
        cfg.archive_dir.clone(),
    );
    let telegram_bot = if cfg.telegram.token.is_empty() {
        None
    } else {
        Some({
            let client = if let Some(proxy) = cfg.socks_proxy {
                default_reqwest_settings()
                    .proxy(reqwest::Proxy::all(dbg!(&proxy))?)
                    .build()?
            } else {
                default_reqwest_settings().build()?
            };
            Bot::with_client(cfg.telegram.token.clone(), client)
        })
    };

    if telegram_bot.is_none() {
        warn!("telegram token is empty; bot worker will not start");
    } else {
        let bot = telegram_bot.clone().expect("bot checked above");
        tokio::spawn(async move {
            loop {
                info!("starting telegram bot worker");
                if let Err(error) = start_bot(state.clone(), bot.clone()).await {
                    error!(error = %error, "telegram bot worker failed");
                } else {
                    warn!("telegram bot worker stopped unexpectedly");
                }

                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
    }

    importer::start(
        &cfg.clickhouse.url,
        &cfg.clickhouse.user,
        &cfg.clickhouse.password,
        &cfg.clickhouse.database,
        &cfg.migrations_dir,
        &cfg.input_dir,
        &cfg.archive_dir,
        Duration::from_secs(cfg.poll_interval_secs),
        telegram_bot,
    )
    .await?;

    Ok(())
}

fn init_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,teloxide=warn"));

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .compact()
        .try_init();
}
