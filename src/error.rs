use axum::{
    Json,
    extract::rejection::{JsonRejection, PathRejection, QueryRejection},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use utoipa::ToSchema;

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug)]
pub enum ApiError {
    BadRequest(String),
    Unauthorized,
    Forbidden(String),
    NotFound(String),
    Conflict(String),
    PayloadTooLarge(String),
    UnsupportedMediaType(String),
    /// Seconds until the key may call again.
    TooManyRequests(u64),
    Internal(anyhow::Error),
}

/// Every error response has this shape.
#[derive(Serialize, ToSchema)]
pub struct ErrorBody {
    /// Machine-readable code: `bad_request`, `unauthorized`, `forbidden`, `not_found`, `conflict`, `too_large`,
    /// `unsupported_type`, `rate_limited` or `internal`.
    pub error: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::BadRequest(msg.into())
    }
    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self::Forbidden(msg.into())
    }
    pub fn not_found(what: &str) -> Self {
        Self::NotFound(format!("{what} not found"))
    }
    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::Conflict(msg.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error, message) = match self {
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, "bad_request", m),
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "missing or invalid API key; send `Authorization: Bearer <key>`".to_owned(),
            ),
            Self::Forbidden(m) => (StatusCode::FORBIDDEN, "forbidden", m),
            Self::NotFound(m) => (StatusCode::NOT_FOUND, "not_found", m),
            Self::Conflict(m) => (StatusCode::CONFLICT, "conflict", m),
            Self::PayloadTooLarge(m) => (StatusCode::PAYLOAD_TOO_LARGE, "too_large", m),
            Self::UnsupportedMediaType(m) => (StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_type", m),
            Self::TooManyRequests(secs) => {
                let body = ErrorBody {
                    error: "rate_limited",
                    message: format!("too many requests for this key; retry in {secs} s"),
                };
                return (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, secs.to_string())], Json(body))
                    .into_response();
            }
            Self::Internal(e) => {
                tracing::error!(error = ?e, "internal error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal", "internal error".to_owned())
            }
        };
        (status, Json(ErrorBody { error, message })).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        if let sqlx::Error::Database(db) = &e {
            match db.code().as_deref() {
                Some("23505") => return Self::conflict("already exists"),
                Some("23514") => return Self::bad_request(format!("invalid value ({})", db.message())),
                _ => {}
            }
        }
        Self::Internal(e.into())
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self::Internal(e)
    }
}

impl From<JsonRejection> for ApiError {
    fn from(r: JsonRejection) -> Self {
        Self::BadRequest(r.body_text())
    }
}

impl From<QueryRejection> for ApiError {
    fn from(r: QueryRejection) -> Self {
        Self::BadRequest(r.body_text())
    }
}

impl From<PathRejection> for ApiError {
    fn from(r: PathRejection) -> Self {
        Self::BadRequest(r.body_text())
    }
}
