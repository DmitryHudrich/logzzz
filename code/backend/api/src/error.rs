use axum::{Json, http::StatusCode, response::IntoResponse};
use domain::error::DomainError;
use serde::Serialize;

#[derive(Serialize, utoipa::ToSchema)]
pub struct ErrorResponse {
    error: String,
}

#[derive(Serialize)]
struct ErrBody {
    error: String,
}

pub enum ApiError {
    BadRequest(String),
    Unauthorized,
    Internal(DomainError),
    InternalMsg(String),
}

impl ApiError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::BadRequest(msg.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        match self {
            Self::BadRequest(msg) => {
                (StatusCode::BAD_REQUEST, Json(ErrBody { error: msg })).into_response()
            }
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                Json(ErrBody {
                    error: "unauthorized".into(),
                }),
            )
                .into_response(),
            Self::Internal(e) => {
                tracing::error!(error = %e, "domain error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrBody {
                        error: "internal server error".into(),
                    }),
                )
                    .into_response()
            }
            Self::InternalMsg(msg) => {
                tracing::error!(error = %msg, "internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrBody {
                        error: "internal server error".into(),
                    }),
                )
                    .into_response()
            }
        }
    }
}
