use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use grammers_client::client::{LoginToken, PasswordToken};
use grammers_client::media::Media;
use grammers_client::{Client, SignInError};
use grammers_mtsender::{ConnectionParams, SenderPool};
use grammers_session::storages::SqliteSession;
use logzz::archive::{
    archive_password_path, build_archive_filename, detect_archive_kind, find_archive_by_message_id,
    partial_archive_path,
};
use logzz::config::{DownloaderCli, DownloaderConfig, load_downloader_config};
use logzz::telegram::{
    ArchiveUploadRequest, format_ready_notification, load_pending_notifications,
    remove_upload_request, save_needs_password_marker, scan_needs_password_archives,
    write_upload_request,
};
use serde::{Deserialize, Serialize};
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::runtime;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, Default, Serialize, Deserialize)]
struct DownloaderState {
    last_downloaded_archive_message_id: i32,
}

enum AuthPhase {
    AwaitingPhone,
    AwaitingCode {
        phone: String,
        token: LoginToken,
    },
    AwaitingPassword {
        hint: Option<String>,
        token: PasswordToken,
    },
    Authorized,
}

struct AuthFlowState {
    phase: AuthPhase,
    last_error: Option<String>,
}

impl AuthFlowState {
    fn awaiting_phone() -> Self {
        Self {
            phase: AuthPhase::AwaitingPhone,
            last_error: None,
        }
    }
}

#[derive(Clone)]
struct AppState {
    cfg: DownloaderConfig,
    auth: Arc<Mutex<AuthFlowState>>,
    runtime: Arc<Mutex<RuntimeState>>,
}

struct RuntimeState {
    client: Option<Arc<Client>>,
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Serialize)]
struct AuthStatusResponse {
    status: &'static str,
    phone: Option<String>,
    password_hint: Option<String>,
    last_error: Option<String>,
    rest_listen_addr: String,
    peer_name: String,
    archive_dir: String,
}

#[derive(Deserialize)]
struct RequestCodePayload {
    phone: String,
}

#[derive(Deserialize)]
struct SubmitCodePayload {
    code: String,
}

#[derive(Deserialize)]
struct SubmitPasswordPayload {
    password: String,
}

#[derive(Serialize)]
struct ApiResponse {
    ok: bool,
    message: String,
}

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
        if !is_authorized(&app_state).await {
            debug!("telegram client is not authorized yet; waiting for REST login");
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            continue;
        }

        let Some(client) = get_client(&app_state).await else {
            warn!("downloader is authorized in state but telegram client is not initialized");
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            continue;
        };

        let peer = match resolved_peer {
            Some(peer) => peer,
            None => {
                let peer = resolve_peer(&client, &cfg.peer_name).await?;
                resolved_peer = Some(peer);
                peer
            }
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

        tokio::time::sleep(std::time::Duration::from_secs(cfg.poll_interval_secs)).await;
    }
}

async fn run_rest_api(state: AppState) {
    let app = Router::new()
        .route("/health", get(health))
        .route("/auth/status", get(auth_status))
        .route("/auth/request-code", post(request_code))
        .route("/auth/submit-code", post(submit_code))
        .route("/auth/submit-password", post(submit_password))
        .route("/auth/reset", post(reset_auth))
        .with_state(state.clone());

    let addr: SocketAddr = match state.cfg.rest_listen_addr.parse() {
        Ok(addr) => addr,
        Err(error) => {
            error!(
                error = %error,
                rest_listen_addr = %state.cfg.rest_listen_addr,
                "invalid downloader REST listen address"
            );
            return;
        }
    };

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(error) => {
            error!(error = %error, rest_listen_addr = %addr, "failed to bind REST server");
            return;
        }
    };

    info!(rest_listen_addr = %addr, "downloader REST API listening");

    if let Err(error) = axum::serve(listener, app).await {
        error!(error = %error, "downloader REST API stopped");
    }
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn auth_status(State(state): State<AppState>) -> Json<AuthStatusResponse> {
    let auth = state.auth.lock().await;
    Json(build_auth_status_response(&state.cfg, &auth))
}

async fn request_code(
    State(state): State<AppState>,
    Json(payload): Json<RequestCodePayload>,
) -> (StatusCode, Json<ApiResponse>) {
    let phone = payload.phone.trim().to_string();
    if phone.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "phone is required");
    }

    let client = match ensure_client(&state).await {
        Ok(client) => client,
        Err(error) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };

    match client.request_login_code(&phone, &state.cfg.api_hash).await {
        Ok(token) => {
            let mut auth = state.auth.lock().await;
            auth.phase = AuthPhase::AwaitingCode { phone, token };
            auth.last_error = None;
            api_ok("login code requested")
        }
        Err(error) => {
            let mut auth = state.auth.lock().await;
            auth.phase = AuthPhase::AwaitingPhone;
            auth.last_error = Some(error.to_string());
            api_error(StatusCode::BAD_REQUEST, &error.to_string())
        }
    }
}

async fn submit_code(
    State(state): State<AppState>,
    Json(payload): Json<SubmitCodePayload>,
) -> (StatusCode, Json<ApiResponse>) {
    let code = payload.code.trim().to_string();
    if code.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "code is required");
    }

    let client = match get_client(&state).await {
        Some(client) => client,
        None => {
            return api_error(
                StatusCode::CONFLICT,
                "telegram client is not initialized; request a login code first",
            );
        }
    };

    let (phone, token) = {
        let mut auth = state.auth.lock().await;
        match std::mem::replace(&mut auth.phase, AuthPhase::AwaitingPhone) {
            AuthPhase::AwaitingCode { phone, token } => {
                auth.last_error = None;
                (phone, token)
            }
            phase => {
                auth.phase = phase;
                return api_error(
                    StatusCode::CONFLICT,
                    "downloader is not waiting for a login code",
                );
            }
        }
    };

    match client.sign_in(&token, &code).await {
        Ok(_) => {
            let mut auth = state.auth.lock().await;
            auth.phase = AuthPhase::Authorized;
            auth.last_error = None;
            api_ok("authorization completed")
        }
        Err(SignInError::PasswordRequired(password_token)) => {
            let hint = password_token.hint().map(str::to_string);
            let mut auth = state.auth.lock().await;
            auth.phase = AuthPhase::AwaitingPassword {
                hint,
                token: password_token,
            };
            auth.last_error = None;
            api_ok("2FA password is required")
        }
        Err(SignInError::InvalidCode) => {
            let mut auth = state.auth.lock().await;
            auth.phase = AuthPhase::AwaitingCode { phone, token };
            auth.last_error = Some("invalid code".to_string());
            api_error(StatusCode::BAD_REQUEST, "invalid code")
        }
        Err(error) => {
            let mut auth = state.auth.lock().await;
            auth.phase = AuthPhase::AwaitingPhone;
            auth.last_error = Some(error.to_string());
            api_error(StatusCode::BAD_REQUEST, &error.to_string())
        }
    }
}

async fn submit_password(
    State(state): State<AppState>,
    Json(payload): Json<SubmitPasswordPayload>,
) -> (StatusCode, Json<ApiResponse>) {
    if payload.password.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "password is required");
    }

    let client = match get_client(&state).await {
        Some(client) => client,
        None => {
            return api_error(
                StatusCode::CONFLICT,
                "telegram client is not initialized; request a login code first",
            );
        }
    };

    let (hint, token) = {
        let mut auth = state.auth.lock().await;
        match std::mem::replace(&mut auth.phase, AuthPhase::AwaitingPhone) {
            AuthPhase::AwaitingPassword { hint, token } => {
                auth.last_error = None;
                (hint, token)
            }
            phase => {
                auth.phase = phase;
                return api_error(
                    StatusCode::CONFLICT,
                    "downloader is not waiting for a 2FA password",
                );
            }
        }
    };

    match client.check_password(token, payload.password).await {
        Ok(_) => {
            let mut auth = state.auth.lock().await;
            auth.phase = AuthPhase::Authorized;
            auth.last_error = None;
            api_ok("2FA authorization completed")
        }
        Err(SignInError::InvalidPassword(password_token)) => {
            let mut auth = state.auth.lock().await;
            auth.phase = AuthPhase::AwaitingPassword {
                hint,
                token: password_token,
            };
            auth.last_error = Some("invalid password".to_string());
            api_error(StatusCode::BAD_REQUEST, "invalid password")
        }
        Err(error) => {
            let mut auth = state.auth.lock().await;
            auth.phase = AuthPhase::AwaitingPhone;
            auth.last_error = Some(error.to_string());
            api_error(StatusCode::BAD_REQUEST, &error.to_string())
        }
    }
}

async fn reset_auth(State(state): State<AppState>) -> (StatusCode, Json<ApiResponse>) {
    let mut auth = state.auth.lock().await;
    auth.phase = AuthPhase::AwaitingPhone;
    auth.last_error = None;
    api_ok("authorization flow reset")
}

fn build_auth_status_response(cfg: &DownloaderConfig, auth: &AuthFlowState) -> AuthStatusResponse {
    let (status, phone, password_hint) = match &auth.phase {
        AuthPhase::AwaitingPhone => ("awaiting_phone", None, None),
        AuthPhase::AwaitingCode { phone, .. } => ("awaiting_code", Some(phone.clone()), None),
        AuthPhase::AwaitingPassword { hint, .. } => ("awaiting_password", None, hint.clone()),
        AuthPhase::Authorized => ("authorized", None, None),
    };

    AuthStatusResponse {
        status,
        phone,
        password_hint,
        last_error: auth.last_error.clone(),
        rest_listen_addr: cfg.rest_listen_addr.clone(),
        peer_name: cfg.peer_name.clone(),
        archive_dir: cfg.archive_dir.clone(),
    }
}

fn api_ok(message: &str) -> (StatusCode, Json<ApiResponse>) {
    (
        StatusCode::OK,
        Json(ApiResponse {
            ok: true,
            message: message.to_string(),
        }),
    )
}

fn api_error(status: StatusCode, message: &str) -> (StatusCode, Json<ApiResponse>) {
    (
        status,
        Json(ApiResponse {
            ok: false,
            message: message.to_string(),
        }),
    )
}

async fn is_authorized(state: &AppState) -> bool {
    let auth = state.auth.lock().await;
    matches!(auth.phase, AuthPhase::Authorized)
}

async fn get_client(state: &AppState) -> Option<Arc<Client>> {
    let runtime = state.runtime.lock().await;
    runtime.client.clone()
}

async fn ensure_client(state: &AppState) -> Result<Arc<Client>> {
    if let Some(client) = get_client(state).await {
        return Ok(client);
    }

    let session_path = PathBuf::from(&state.cfg.session_file);
    ensure_parent_dir(&session_path).await?;
    let client = initialize_client(&state.cfg, &session_path).await?;

    let mut runtime = state.runtime.lock().await;
    if let Some(existing) = runtime.client.clone() {
        return Ok(existing);
    }
    runtime.client = Some(client.clone());
    Ok(client)
}

async fn initialize_client(cfg: &DownloaderConfig, session_path: &Path) -> Result<Arc<Client>> {
    let session = Arc::new(SqliteSession::open(session_path).await?);
    let params = ConnectionParams {
        proxy_url: cfg.socks_proxy.clone(),
        ..Default::default()
    };
    let SenderPool { runner, handle, .. } =
        SenderPool::with_configuration(Arc::clone(&session), cfg.api_id, params);
    let client = Arc::new(Client::new(handle));
    tokio::spawn(runner.run());
    Ok(client)
}

async fn resolve_peer(
    client: &Client,
    peer_name: &str,
) -> Result<grammers_session::types::PeerRef> {
    let maybe_peer = client
        .resolve_username(peer_name)
        .await?
        .ok_or("no peer with username")?
        .to_ref()
        .await;

    Ok(maybe_peer.unwrap_or_else(|| panic!("Peer {peer_name} could not be found")))
}

async fn sync_new_archives(
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
            if !text.is_empty() {
                if let Some(reply_to_id) = msg.reply_to_message_id() {
                    password_replies.push((reply_to_id, text.trim().to_string()));
                }
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

        let archive_kind = detect_archive_kind(Path::new(original_name)).expect("archive checked");
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

        let peer1 = message.peer_ref().await.unwrap();
        let msg = client.send_message(peer1, "Download started").await?;
        let upload_request =
            ArchiveUploadRequest::for_userbot(peer_name, message_id, msg.id(), original_name);
        write_upload_request(&final_path, &upload_request).await?;

        let mut file = tokio::fs::File::create(&temp_path).await?;
        let mut downloaded_bytes = 0usize;
        let total = document.size().unwrap() as f64;

        let mut iteration = 0usize;

        while let Some(chunk) = download.next().await.map_err(io::Error::other)? {
            file.write_all(&chunk).await?;
            downloaded_bytes += chunk.len();

            let progress = downloaded_bytes as f64 / total * 100.0;
            iteration += 1;

            if iteration % 5 == 0 {
                client
                    .edit_message(peer1, msg.id(), format!("Downloaded {:.2}%", progress))
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
                format!("Downloaded {}. Waiting for parsing.", original_name),
            )
            .await?;

        state.last_downloaded_archive_message_id = message_id;
        save_state(state_path, state).await?;
        downloaded += 1;
    }

    Ok(downloaded)
}

async fn flush_needs_password_notifications(
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

async fn flush_parse_notifications(
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

async fn load_state(path: &Path) -> Result<DownloaderState> {
    match tokio::fs::read(path).await {
        Ok(raw) => Ok(serde_json::from_slice(&raw)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(DownloaderState::default()),
        Err(error) => Err(Box::new(error)),
    }
}

async fn save_state(path: &Path, state: &DownloaderState) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent).await?;
    }

    let temp_path = path.with_extension("tmp");
    let payload = serde_json::to_vec_pretty(state)?;
    tokio::fs::write(&temp_path, payload).await?;
    tokio::fs::rename(&temp_path, path).await?;
    Ok(())
}

fn is_archive_name(name: &str) -> bool {
    detect_archive_kind(Path::new(name)).is_some()
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .compact()
        .try_init();
}

async fn ensure_parent_dir(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent).await?;
    }

    Ok(())
}

fn main() -> Result<()> {
    runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async_main())
}
