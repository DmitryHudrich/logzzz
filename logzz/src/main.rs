use clap::Parser;
use clickhouse::Client;
use eyre::Result;
use std::sync::Arc;
use std::time::Duration;
use teloxide::Bot;
use teloxide::net::default_reqwest_settings;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

use logzz::application::events::EventBus;
use logzz::application::ingest::{IngestService, ImportStatus};
use logzz::application::search::SearchService;
use logzz::application::sources::SourceScheduler;
use logzz::domain::repository::CredentialRepository;
use logzz::domain::source::ArchiveInbox;
use logzz::infrastructure::archive::{check_extractors, sanitize_filename};
use logzz::infrastructure::clickhouse::{ClickhouseCredentialRepository, run_migrations};
use logzz::infrastructure::inbox::FsArchiveInbox;
use logzz::infrastructure::sources::LocalDirectorySource;
use logzz::interface::bot::{
    BotState, flush_password_request_notifications, flush_ready_notifications, start_bot,
};
use logzz::interface::config::{Cli, load_config};
use logzz::interface::rest::{RestState, run_rest_api};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let cli = Cli::parse();
    let cfg = load_config(&cli)?;

    let bootstrap = Client::default()
        .with_url(&cfg.clickhouse.url)
        .with_user(&cfg.clickhouse.user)
        .with_password(&cfg.clickhouse.password)
        .with_database(&cfg.clickhouse.database);
    bootstrap
        .query(&format!(
            "CREATE DATABASE IF NOT EXISTS {}",
            cfg.clickhouse.database
        ))
        .execute()
        .await?;

    let client = Arc::new(
        Client::default()
            .with_url(&cfg.clickhouse.url)
            .with_user(&cfg.clickhouse.user)
            .with_password(&cfg.clickhouse.password)
            .with_database(&cfg.clickhouse.database),
    );

    run_migrations(&client, &cfg.migrations_dir).await?;

    let repo: Arc<dyn CredentialRepository> =
        Arc::new(ClickhouseCredentialRepository::new(client.clone()));
    let search = SearchService::new(repo.clone());

    let input_dir = cfg.input_dir.clone();
    let archive_dir = cfg.archive_dir.clone();
    tokio::fs::create_dir_all(&input_dir).await?;
    tokio::fs::create_dir_all(&archive_dir).await?;

    report_extractors();

    if cfg.telegram.allowed_user_ids.is_empty() {
        warn!(
            "LOGZZ_TELEGRAM__ALLOWED_USER_IDS is not set; the search bot will respond to ANY \
             telegram user, exposing the full imported credential database. Set it to a \
             comma-separated list of trusted telegram user ids to restrict access."
        );
    }

    let telegram_bot = if cfg.telegram.token.is_empty() {
        warn!("telegram token is empty; bot worker will not start");
        None
    } else {
        let http_client = if let Some(proxy) = cfg.socks_proxy.clone() {
            default_reqwest_settings()
                .proxy(reqwest::Proxy::all(&proxy)?)
                .build()?
        } else {
            default_reqwest_settings().build()?
        };
        Some(Bot::with_client(cfg.telegram.token.clone(), http_client))
    };

    if let Some(bot) = telegram_bot.clone() {
        let bot_state = BotState::new(
            search.clone(),
            cfg.telegram.results_dir.clone(),
            input_dir.clone(),
            archive_dir.clone(),
            cfg.telegram.allowed_user_ids.clone(),
        );
        tokio::spawn(async move {
            loop {
                info!("starting telegram bot worker");
                if let Err(error) = start_bot(bot_state.clone(), bot.clone()).await {
                    error!(error = %error, "telegram bot worker failed");
                } else {
                    warn!("telegram bot worker stopped unexpectedly");
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
    }

    let inbox: Arc<dyn ArchiveInbox> = Arc::new(FsArchiveInbox::new(&archive_dir));
    let mut scheduler = SourceScheduler::new(inbox.clone());
    for (index, dir) in cfg.local_source_dirs.iter().enumerate() {
        let name = format!("local:{}", sanitize_filename(dir));
        let state_file = format!(
            "./.local/sources/{}-{:03}.state.json",
            sanitize_filename(dir),
            index
        );
        let source: Arc<dyn logzz::domain::source::LogSource> =
            Arc::new(LocalDirectorySource::new(name, dir.clone(), state_file).await);
        scheduler.register(source);
    }

    let status = Arc::new(Mutex::new(ImportStatus::default()));
    let events = EventBus::default();
    let ingest = IngestService::new(
        repo.clone(),
        input_dir.clone(),
        archive_dir.clone(),
        status.clone(),
        events.clone(),
    );

    if let Some(listen_addr) = cfg.rest.listen_addr.clone() {
        if cfg.rest.api_token.is_none() {
            warn!(
                "LOGZZ_REST__API_TOKEN is not set; the REST API accepts requests from anyone who \
                 can reach the listen address with no credential. Set a token and keep the port \
                 bound to a trusted network."
            );
        }
        let rest_state = RestState {
            search: search.clone(),
            repo: repo.clone(),
            inbox: inbox.clone(),
            status: status.clone(),
            api_token: cfg.rest.api_token.clone(),
            source_names: scheduler.source_names(),
            events: events.clone(),
            archive_dir: std::path::PathBuf::from(&archive_dir),
        };
        tokio::spawn(run_rest_api(listen_addr, rest_state));
    } else {
        info!("REST API disabled (set LOGZZ_REST__LISTEN_ADDR to enable)");
    }

    let poll_interval = Duration::from_secs(cfg.poll_interval_secs);
    let archive_dir_path = std::path::PathBuf::from(&archive_dir);

    info!(
        input_dir = %input_dir,
        archive_dir = %archive_dir,
        poll_interval_secs = cfg.poll_interval_secs,
        sources = scheduler.len(),
        "importer daemon started"
    );

    loop {
        if !scheduler.is_empty() {
            scheduler.poll_all().await;
        }

        match ingest.run_cycle().await {
            Ok(stats) => {
                if stats.did_work() {
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
            Err(error) => error!(error = %error, "import cycle failed"),
        }

        if let Some(bot) = telegram_bot.as_ref() {
            match flush_password_request_notifications(bot, &archive_dir_path).await {
                Ok(sent) if sent > 0 => {
                    info!(sent, "telegram needs-password notifications delivered")
                }
                Ok(_) => {}
                Err(error) => {
                    warn!(error = %error, "failed to flush needs-password notifications")
                }
            }

            match flush_ready_notifications(bot, &archive_dir_path).await {
                Ok(sent) if sent > 0 => info!(sent, "telegram archive notifications delivered"),
                Ok(_) => {}
                Err(error) => {
                    warn!(error = %error, "failed to flush telegram archive notifications")
                }
            }
        }

        tokio::time::sleep(poll_interval).await;
    }
}

fn report_extractors() {
    let extractors = check_extractors();
    if extractors.rar_supported() {
        info!(
            rar_extractors = ?extractors.rar_tools,
            "archive extractors detected (zip: in-process, rar: external)"
        );
    } else {
        warn!(
            "no RAR extractor found on PATH; .rar archives cannot be extracted and will keep \
             failing on every retry. Install `unrar` or `7z` (p7zip-full). ZIP archives are \
             still supported."
        );
    }
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
