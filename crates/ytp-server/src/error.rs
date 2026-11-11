//! API errors: `{ "error": "<code>", "message": "<text>" }`, plus extractor
//! wrappers whose rejections use the same shape.

use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::de::DeserializeOwned;
use serde_json::json;
use ytp_core::Error;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "authentication required",
        )
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }

    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "not found")
    }

    pub fn not_implemented() -> Self {
        Self::new(
            StatusCode::NOT_IMPLEMENTED,
            "not_implemented",
            "not available yet",
        )
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }
}

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        use StatusCode as S;
        let msg = e.to_string();
        match e {
            Error::BadRequest(_) => Self::new(S::BAD_REQUEST, "bad_request", msg),
            Error::Unauthorized => Self::unauthorized(),
            Error::Forbidden(_) => Self::new(S::FORBIDDEN, "forbidden", msg),
            Error::NotFound(_) => Self::new(S::NOT_FOUND, "not_found", msg),
            Error::Conflict(_) => Self::new(S::CONFLICT, "conflict", msg),
            Error::Invalid(_) => Self::new(S::UNPROCESSABLE_ENTITY, "invalid", msg),
            Error::RateLimited => Self::new(
                S::TOO_MANY_REQUESTS,
                "rate_limited",
                "too many attempts, try again in a minute",
            ),
            Error::TooLarge(_) => Self::new(S::BAD_REQUEST, "too_large", msg),
            Error::NotImplemented => Self::not_implemented(),
            Error::Internal(detail) => {
                tracing::error!(%detail, "internal error");
                Self::new(
                    S::INTERNAL_SERVER_ERROR,
                    "internal",
                    "internal server error",
                )
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({ "error": self.code, "message": self.message })),
        )
            .into_response()
    }
}

/// `Json<T>` whose rejections are API errors (bad JSON 400, wrong shape 422).
pub struct ApiJson<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for ApiJson<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(v)) => Ok(ApiJson(v)),
            Err(JsonRejection::JsonDataError(e)) => Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid",
                e.body_text(),
            )),
            Err(e) => Err(ApiError::bad_request(e.body_text())),
        }
    }
}

pub struct ApiPath<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned + Send> FromRequestParts<S> for ApiPath<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        axum::extract::Path::<T>::from_request_parts(parts, state)
            .await
            .map(|p| ApiPath(p.0))
            .map_err(|e: PathRejection| ApiError::bad_request(e.body_text()))
    }
}

pub struct ApiQuery<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequestParts<S> for ApiQuery<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        axum::extract::Query::<T>::from_request_parts(parts, state)
            .await
            .map(|q| ApiQuery(q.0))
            .map_err(|e: QueryRejection| ApiError::bad_request(e.body_text()))
    }
}
