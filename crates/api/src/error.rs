//! HTTP error mapping.
//!
//! `anyhow` is fine at this boundary because everything here is
//! about to become a status code anyway. What matters is that domain errors
//! arrive *structured* — `ledger::LedgerError`, `billing::RatingError` — so
//! this layer can decide which are the caller's fault and which are ours.
//! A library that returned `anyhow::Error` would have made that impossible.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::auth::Scope;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("authentication required")]
    Unauthenticated,
    #[error("missing required scope: {missing_scope}")]
    Forbidden { missing_scope: Scope },
    #[error("{0} not found")]
    NotFound(&'static str),
    #[error("{0}")]
    BadRequest(String),
    /// A domain rule rejected the request — an unbalanced entry, an exhausted
    /// commitment. The caller asked for something the business does not allow,
    /// which is a 422, not a 500.
    #[error("{0}")]
    Unprocessable(String),
    #[error("internal error")]
    Internal(#[from] anyhow::Error),
}

#[derive(serde::Serialize)]
struct ErrorBody {
    error: String,
    message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            Self::Unauthenticated => (StatusCode::UNAUTHORIZED, "unauthenticated"),
            Self::Forbidden { .. } => (StatusCode::FORBIDDEN, "forbidden"),
            Self::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            Self::Unprocessable(_) => (StatusCode::UNPROCESSABLE_ENTITY, "unprocessable"),
            Self::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        };

        // Internal errors are logged in full and reported as a bare string.
        // A storage error can carry a query, a constraint name, or a fragment
        // of another tenant's data in its message; none of that belongs in a
        // response body.
        let message = match &self {
            Self::Internal(e) => {
                tracing::error!(error = ?e, "request failed");
                "an internal error occurred".to_owned()
            }
            other => other.to_string(),
        };

        (
            status,
            Json(ErrorBody {
                error: code.to_owned(),
                message,
            }),
        )
            .into_response()
    }
}

/// Maps a domain error into the 422 bucket, preserving its message.
///
/// Domain errors are safe to surface: they are written for the caller and
/// contain no storage internals.
pub fn domain<E: core::fmt::Display>(e: E) -> ApiError {
    ApiError::Unprocessable(e.to_string())
}

/// Maps a storage error into the 500 bucket.
pub fn storage<E: core::fmt::Display>(e: E) -> ApiError {
    ApiError::Internal(anyhow::anyhow!("{e}"))
}
