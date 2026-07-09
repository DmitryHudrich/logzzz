use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header::AUTHORIZATION};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use grammers_client::client::{LoginToken, PasswordToken};
use grammers_client::{Client, SignInError};
use grammers_mtsender::{ConnectionParams, SenderPool};
use grammers_session::storages::SqliteSession;
use logzz::config::DownloaderConfig;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info};

use crate::Result;

pub enum AuthPhase {
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

pub struct AuthFlowState {
    pub phase: AuthPhase,
    pub last_error: Option<String>,
}

impl AuthFlowState {
    pub fn awaiting_phone() -> Self {
        Self {
            phase: AuthPhase::AwaitingPhone,
            last_error: None,
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub cfg: DownloaderConfig,
    pub auth: Arc<Mutex<AuthFlowState>>,
    pub runtime: Arc<Mutex<RuntimeState>>,
}

pub struct RuntimeState {
    pub client: Option<Arc<Client>>,
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

pub async fn run_rest_api(state: AppState) {
    let auth_routes = Router::new()
        .route("/auth/status", get(auth_status))
        .route("/auth/request-code", post(request_code))
        .route("/auth/submit-code", post(submit_code))
        .route("/auth/submit-password", post(submit_password))
        .route("/auth/reset", post(reset_auth))
        .layer(middleware::from_fn_with_state(state.clone(), require_token));

    let app = Router::new()
        .route("/health", get(health))
        .merge(auth_routes)
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

/// Requires a `Authorization: Bearer <token>` header matching `cfg.rest_api_token` on
/// every `/auth/*` route. If no token is configured, the API stays open (with a
/// startup warning logged elsewhere) so existing single-operator deployments keep working.
async fn require_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    let Some(expected) = state.cfg.rest_api_token.as_deref() else {
        return next.run(request).await;
    };

    let provided = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));

    if provided == Some(expected) {
        next.run(request).await
    } else {
        api_error(StatusCode::UNAUTHORIZED, "missing or invalid bearer token").into_response()
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

pub async fn is_authorized(state: &AppState) -> bool {
    let auth = state.auth.lock().await;
    matches!(auth.phase, AuthPhase::Authorized)
}

pub async fn get_client(state: &AppState) -> Option<Arc<Client>> {
    let runtime = state.runtime.lock().await;
    runtime.client.clone()
}

async fn ensure_client(state: &AppState) -> Result<Arc<Client>> {
    if let Some(client) = get_client(state).await {
        return Ok(client);
    }

    let session_path = Path::new(&state.cfg.session_file);
    ensure_parent_dir(session_path).await?;
    let client = initialize_client(&state.cfg, session_path).await?;

    let mut runtime = state.runtime.lock().await;
    if let Some(existing) = runtime.client.clone() {
        return Ok(existing);
    }
    runtime.client = Some(client.clone());
    Ok(client)
}

pub async fn initialize_client(cfg: &DownloaderConfig, session_path: &Path) -> Result<Arc<Client>> {
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

pub async fn ensure_parent_dir(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent).await?;
    }

    Ok(())
}
