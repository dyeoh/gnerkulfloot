//! The one place errors become HTTP responses. Every handler returns
//! `Result<_, AppError>`, and `AppError` renders RFC 7807
//! `application/problem+json`. Internal details are logged, never sent to clients.

use axum::{
    Json,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    BadRequest(String),
    #[error("authentication required")]
    Unauthorized,
    #[error("not allowed")]
    Forbidden,
    #[error("{0}")]
    Conflict(String),
    /// A rejection with machine-readable details for the client, added to the
    /// problem body as extra members (e.g. `{"sku_id": …, "available": 2}`).
    #[error("{detail}")]
    Rejected {
        status: StatusCode,
        detail: String,
        extra: serde_json::Map<String, serde_json::Value>,
    },
    #[error("{0}")]
    UnsupportedMediaType(String),
    #[error("too many requests")]
    RateLimited { retry_after_secs: u64 },
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl From<sqlx::Error> for AppError {
    fn from(err: sqlx::Error) -> Self {
        match err {
            sqlx::Error::RowNotFound => AppError::NotFound,
            other => AppError::Internal(other.into()),
        }
    }
}

/// Errors from simple admin-managed reference data (shipping zones and rates,
/// tax rules), which only ever fail in these few ways.
#[derive(Debug, thiserror::Error)]
pub enum DataError {
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    InvalidInput(String),
    #[error("{0}")]
    Conflict(String),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

impl From<DataError> for AppError {
    fn from(err: DataError) -> Self {
        match err {
            DataError::NotFound => AppError::NotFound,
            DataError::InvalidInput(msg) => AppError::BadRequest(msg),
            DataError::Conflict(msg) => AppError::Conflict(msg),
            DataError::Database(e) => e.into(),
        }
    }
}

/// RFC 7807 problem details body.
#[derive(Serialize)]
struct Problem {
    #[serde(rename = "type")]
    kind: &'static str,
    title: &'static str,
    status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

impl AppError {
    fn status(&self) -> StatusCode {
        match self {
            AppError::NotFound => StatusCode::NOT_FOUND,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::Forbidden => StatusCode::FORBIDDEN,
            AppError::Conflict(_) => StatusCode::CONFLICT,
            AppError::Rejected { status, .. } => *status,
            AppError::UnsupportedMediaType(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            AppError::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        let detail = match &self {
            AppError::BadRequest(msg) | AppError::Conflict(msg) | AppError::UnsupportedMediaType(msg) => {
                Some(msg.clone())
            }
            AppError::Rejected { detail, .. } => Some(detail.clone()),
            AppError::Internal(err) => {
                // Logged inside the request span, so the log line carries the request id
                // the client sees in `x-request-id`.
                tracing::error!(error = ?err, "internal error");
                None
            }
            _ => None,
        };
        let body = Problem {
            kind: "about:blank",
            title: status.canonical_reason().unwrap_or("error"),
            status: status.as_u16(),
            detail,
            extra: match &self {
                AppError::Rejected { extra, .. } => extra.clone(),
                _ => Default::default(),
            },
        };
        let mut res = (status, [(header::CONTENT_TYPE, "application/problem+json")], Json(body)).into_response();
        match self {
            AppError::RateLimited { retry_after_secs } => {
                res.headers_mut()
                    .insert(header::RETRY_AFTER, retry_after_secs.max(1).into());
            }
            AppError::Unauthorized => {
                res.headers_mut()
                    .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            }
            _ => {}
        }
        res
    }
}
