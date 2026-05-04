use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl AppError {
    pub fn not_found(msg: impl Into<String>) -> Self {
        AppError::Message(msg.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            AppError::Message(_) => StatusCode::NOT_FOUND,
            AppError::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = format!(
            "<!DOCTYPE html><html><body><h1>Error</h1><p>{}</p></body></html>",
            self
        );
        (
            status,
            [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
            body,
        )
            .into_response()
    }
}
