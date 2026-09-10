mod dto;

use axum::extract::{DefaultBodyLimit, Multipart, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header::AUTHORIZATION};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tracing::{error, info};

use crate::application::events::{AppEvent, EventBus};
use crate::application::ingest::SharedImportStatus;
use crate::application::search::SearchService;
use crate::domain::credential_key;
use crate::domain::repository::{CredentialRepository, SearchType};
use crate::domain::source::ArchiveInbox;
use crate::infrastructure::archive::{
    archive_needs_password_path, archive_password_path, is_archive_file,
};
use dto::{
    ApiError, CredentialDto, HealthResponse, MetricsResponse, OkResponse, SearchResponse,
    TagListResponse, TagsRequest, UploadResponse,
};

const MAX_UPLOAD_BYTES: usize = 2 * 1024 * 1024 * 1024;
const INDEX_HTML: &str = include_str!("../../../../frontend/index.html");

#[derive(Clone)]
pub struct RestState {
    pub search: SearchService,
    pub repo: Arc<dyn CredentialRepository>,
    pub inbox: Arc<dyn ArchiveInbox>,
    pub status: SharedImportStatus,
    pub api_token: Option<String>,
    pub source_names: Vec<String>,
    pub events: EventBus,
    pub archive_dir: PathBuf,
}

pub async fn run_rest_api(listen_addr: String, state: RestState) {
    let protected = Router::new()
        .route("/metrics", get(metrics))
        .route("/api/search", get(search))
        .route("/api/import/status", get(import_status))
        .route("/api/archives", post(upload_archive))
        .route("/api/archives/queue", get(archive_queue))
        .route("/api/archives/password", post(submit_archive_password))
        .route("/api/tags", get(list_tags).post(add_tags).delete(remove_tags))
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES))
        .layer(middleware::from_fn_with_state(state.clone(), require_token));

    let app = Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .route("/health", get(health))
        .route("/api/events", get(events))
        .merge(protected)
        .with_state(state);

    let addr: SocketAddr = match listen_addr.parse() {
        Ok(addr) => addr,
        Err(error) => {
            error!(error = %error, listen_addr = %listen_addr, "invalid REST listen address");
            return;
        }
    };

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(error) => {
            error!(error = %error, listen_addr = %addr, "failed to bind REST server");
            return;
        }
    };

    info!(listen_addr = %addr, "logzz REST API listening");

    if let Err(error) = axum::serve(listener, app).await {
        error!(error = %error, "logzz REST API stopped");
    }
}

async fn require_token(
    State(state): State<RestState>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    let Some(expected) = state.api_token.as_deref() else {
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

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

#[derive(Deserialize)]
struct EventsParams {
    #[serde(default)]
    token: Option<String>,
}

async fn events(State(state): State<RestState>, Query(params): Query<EventsParams>) -> Response {
    if let Some(expected) = state.api_token.as_deref()
        && params.token.as_deref() != Some(expected)
    {
        return api_error(StatusCode::UNAUTHORIZED, "missing or invalid token").into_response();
    }

    let stream = BroadcastStream::new(state.events.subscribe()).filter_map(|item| {
        item.ok()
            .and_then(|event| Event::default().json_data(event).ok())
            .map(Ok::<Event, Infallible>)
    });

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn metrics(State(state): State<RestState>) -> Response {
    let store = match state.repo.metrics().await {
        Ok(metrics) => metrics,
        Err(error) => {
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string())
                .into_response();
        }
    };
    let status = state.status.lock().await.clone();
    Json(MetricsResponse::new(store, status, state.source_names.clone())).into_response()
}

async fn import_status(State(state): State<RestState>) -> Response {
    let status = state.status.lock().await.clone();
    Json(status).into_response()
}

#[derive(Deserialize)]
struct SearchParams {
    #[serde(default)]
    q: Option<String>,
    #[serde(default, rename = "type")]
    search_type: Option<String>,
    #[serde(default)]
    tags: Option<String>,
    #[serde(default)]
    exclude_tags: Option<String>,
    #[serde(default)]
    complete: Option<String>,
    #[serde(default)]
    page: Option<usize>,
}

fn is_truthy(value: Option<&str>) -> bool {
    matches!(value, Some("1" | "true" | "on" | "yes"))
}

fn parse_tag_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

async fn search(State(state): State<RestState>, Query(params): Query<SearchParams>) -> Response {
    let term = params.q.unwrap_or_default().trim().to_string();
    let tags = params.tags.as_deref().map(parse_tag_list).unwrap_or_default();
    let exclude_tags = params
        .exclude_tags
        .as_deref()
        .map(parse_tag_list)
        .unwrap_or_default();

    if term.is_empty() && tags.is_empty() && exclude_tags.is_empty() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "at least one of `q`, `tags` or `exclude_tags` is required",
        )
        .into_response();
    }

    let search_type = SearchType::from_str_lossy(params.search_type.as_deref().unwrap_or("url"));
    let require_complete = is_truthy(params.complete.as_deref());
    let page = params.page.unwrap_or(0);

    match state
        .search
        .search(&term, search_type, &tags, &exclude_tags, require_complete, page)
        .await
    {
        Ok(result) => {
            let records: Vec<CredentialDto> =
                result.records.into_iter().map(CredentialDto::from).collect();
            Json(SearchResponse {
                query: term,
                search_type: search_type.as_str().to_string(),
                tags,
                page: result.page,
                has_next: result.has_next,
                total_unique: result.total_unique,
                count: records.len(),
                records,
            })
            .into_response()
        }
        Err(error) => {
            api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()).into_response()
        }
    }
}

async fn list_tags(State(state): State<RestState>) -> Response {
    match state.repo.all_tags().await {
        Ok(tags) => Json(TagListResponse { tags }).into_response(),
        Err(error) => {
            api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()).into_response()
        }
    }
}

async fn add_tags(State(state): State<RestState>, Json(req): Json<TagsRequest>) -> Response {
    mutate_tags(state, req, true).await
}

async fn remove_tags(State(state): State<RestState>, Json(req): Json<TagsRequest>) -> Response {
    mutate_tags(state, req, false).await
}

async fn mutate_tags(state: RestState, req: TagsRequest, add: bool) -> Response {
    let key = credential_key(&req.url, &req.username, &req.password);

    let outcome = if add {
        state.repo.add_tags(&key, &req.tags).await
    } else {
        state.repo.remove_tags(&key, &req.tags).await
    };

    if let Err(error) = outcome {
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()).into_response();
    }

    match state.repo.tags_for_keys(std::slice::from_ref(&key)).await {
        Ok(mut map) => Json(OkResponse {
            ok: true,
            tags: map.remove(&key).unwrap_or_default(),
            cred_key: key,
        })
        .into_response(),
        Err(error) => {
            api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()).into_response()
        }
    }
}

async fn upload_archive(State(state): State<RestState>, mut multipart: Multipart) -> Response {
    let field = match multipart.next_field().await {
        Ok(Some(field)) => field,
        Ok(None) => {
            return api_error(StatusCode::BAD_REQUEST, "expected a `file` multipart field")
                .into_response();
        }
        Err(error) => {
            return api_error(StatusCode::BAD_REQUEST, &error.to_string()).into_response();
        }
    };

    let original_name = field
        .file_name()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "archive".to_string());

    let temp_path = std::env::temp_dir().join(format!(
        "logzz-upload-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));

    let mut file = match tokio::fs::File::create(&temp_path).await {
        Ok(file) => file,
        Err(error) => {
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string())
                .into_response();
        }
    };

    let mut field = field;
    loop {
        match field.chunk().await {
            Ok(Some(chunk)) => {
                if let Err(error) = file.write_all(&chunk).await {
                    let _ = tokio::fs::remove_file(&temp_path).await;
                    return api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string())
                        .into_response();
                }
            }
            Ok(None) => break,
            Err(error) => {
                let _ = tokio::fs::remove_file(&temp_path).await;
                return api_error(StatusCode::BAD_REQUEST, &error.to_string()).into_response();
            }
        }
    }

    if let Err(error) = file.flush().await {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()).into_response();
    }
    drop(file);

    match state.inbox.deposit(&temp_path, &original_name).await {
        Ok(final_path) => {
            let name = final_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&original_name)
                .to_string();
            state.events.emit(AppEvent::queued(name));
            Json(UploadResponse {
                queued: true,
                path: final_path.display().to_string(),
            })
            .into_response()
        }
        Err(error) => {
            let _ = tokio::fs::remove_file(&temp_path).await;
            api_error(StatusCode::BAD_REQUEST, &error.to_string()).into_response()
        }
    }
}

async fn archive_queue(State(state): State<RestState>) -> Response {
    let mut reader = match tokio::fs::read_dir(&state.archive_dir).await {
        Ok(reader) => reader,
        Err(_) => return Json(serde_json::json!({ "archives": [] })).into_response(),
    };

    let mut items: Vec<serde_json::Value> = Vec::new();
    while let Ok(Some(entry)) = reader.next_entry().await {
        let path = entry.path();
        if !is_archive_file(&path) {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let status = if archive_password_path(&path).exists() {
            "password_submitted"
        } else if archive_needs_password_path(&path).exists() {
            "needs_password"
        } else {
            "queued"
        };
        items.push(serde_json::json!({ "name": name, "status": status }));
    }

    items.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Json(serde_json::json!({ "archives": items })).into_response()
}

#[derive(Deserialize)]
struct ArchivePasswordRequest {
    archive: String,
    password: String,
}

async fn submit_archive_password(
    State(state): State<RestState>,
    Json(req): Json<ArchivePasswordRequest>,
) -> Response {
    let file_name = Path::new(&req.archive)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if file_name.is_empty() || file_name != req.archive {
        return api_error(StatusCode::BAD_REQUEST, "invalid archive name").into_response();
    }
    if req.password.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "password is required").into_response();
    }

    let archive_path = state.archive_dir.join(file_name);
    if !archive_path.exists() {
        return api_error(StatusCode::NOT_FOUND, "archive is no longer awaiting a password")
            .into_response();
    }

    let pass_path = archive_password_path(&archive_path);
    match tokio::fs::write(&pass_path, req.password.as_bytes()).await {
        Ok(()) => Json(HealthResponse { status: "ok" }).into_response(),
        Err(error) => {
            api_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()).into_response()
        }
    }
}

fn api_error(status: StatusCode, message: &str) -> (StatusCode, Json<ApiError>) {
    (
        status,
        Json(ApiError {
            ok: false,
            message: message.to_string(),
        }),
    )
}
