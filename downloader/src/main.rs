mod rest;
mod state;
mod sync;

use clap::Parser;
use logzz::config::{DownloaderCli, load_downloader_config};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

use rest::{AppState, AuthFlowState, AuthPhase, RuntimeState, ensure_parent_dir, initialize_client, run_rest_api};
use state::load_state;
use sync::{
    flush_needs_password_notifications, flush_parse_notifications, resolve_peer,
    sync_new_archives,
};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

async fn async_main() -> Result<()> {
    init_tracing();

    let cli = DownloaderCli::parse();
    let cfg = load_downloader_config(&cli)?;
    let archive_dir = PathBuf::from(&cfg.archive_dir);
    let state_path = PathBuf::from(&cfg.state_file);
    let session_path = PathBuf::from(&cfg.session_file);

    tokio::fs::create_dir_all(&archive_dir).await?;
    ensure_parent_dir(&state_path).await?;
    ensure_parent_dir(&session_path).await?;

    if cfg.rest_api_token.is_none() {
        warn!(
            "DOWNLOADER_REST_API_TOKEN is not set; the REST auth API (/auth/*) accepts requests \
             from anyone who can reach rest_listen_addr with no credential. Set a token, and keep \
             this service bound to localhost/a trusted network."
        );
    }

    let session_exists = tokio::fs::try_exists(&session_path).await.unwrap_or(false);
    let (auth_state, runtime_state) = if session_exists {
        let client = initialize_client(&cfg, &session_path).await?;
        let auth_state = if client.is_authorized().await? {
            AuthFlowState {
                phase: AuthPhase::Authorized,
                last_error: None,
            }
        } else {
            AuthFlowState::awaiting_phone()
        };
        (
            auth_state,
            RuntimeState {
                client: Some(client),
            },
        )
    } else {
        (
            AuthFlowState::awaiting_phone(),
            RuntimeState { client: None },
        )
    };

    let app_state = AppState {
        cfg: cfg.clone(),
        auth: Arc::new(Mutex::new(auth_state)),
        runtime: Arc::new(Mutex::new(runtime_state)),
    };

    tokio::spawn(run_rest_api(app_state.clone()));

    info!(
        peer = %cfg.peer_name,
        archive_dir = %archive_dir.display(),
        state_path = %state_path.display(),
        poll_interval_secs = cfg.poll_interval_secs,
        session_file = %cfg.session_file,
        rest_listen_addr = %cfg.rest_listen_addr,
        "downloader started"
    );

    let mut resolved_peer = None;
    let mut state = load_state(&state_path).await?;

    loop {
        if !rest::is_authorized(&app_state).await {
            debug!("telegram client is not authorized yet; waiting for REST login");
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }

        let Some(client) = rest::get_client(&app_state).await else {
            warn!("downloader is authorized in state but telegram client is not initialized");
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        };

        let peer = match resolved_peer {
            Some(peer) => peer,
            None => match resolve_peer(&client, &cfg.peer_name).await {
                Ok(peer) => {
                    resolved_peer = Some(peer);
                    peer
                }
                Err(error) => {
                    error!(error = %error, peer_name = %cfg.peer_name, "failed to resolve peer; retrying");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            },
        };

        match sync_new_archives(
            &client,
            peer,
            &cfg.peer_name,
            &archive_dir,
            &state_path,
            &mut state,
        )
        .await
        {
            Ok(downloaded) => {
                if downloaded == 0 {
                    debug!("no new archives found");
                } else {
                    info!(downloaded, "downloaded new archives");
                }
            }
            Err(error) => {
                error!(error = %error, "archive sync failed");
                resolved_peer = None;
            }
        }

        match flush_needs_password_notifications(&client, peer, &cfg.peer_name, &archive_dir).await
        {
            Ok(sent) if sent > 0 => {
                info!(sent, "needs-password notifications delivered to userbot");
            }
            Ok(_) => {}
            Err(error) => {
                warn!(error = %error, "failed to flush needs-password notifications");
            }
        }

        match flush_parse_notifications(&client, peer, &cfg.peer_name, &archive_dir).await {
            Ok(updated) if updated > 0 => {
                info!(updated, "parse notifications delivered to userbot");
            }
            Ok(_) => {}
            Err(error) => {
                warn!(error = %error, "failed to flush parse notifications");
            }
        }

        tokio::time::sleep(Duration::from_secs(cfg.poll_interval_secs)).await;
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .compact()
        .try_init();
}

fn main() -> Result<()> {
    runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async_main())
}
