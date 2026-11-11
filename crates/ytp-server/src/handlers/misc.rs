use axum::Json;
use serde_json::{json, Value};

use crate::auth::CurrentUser;
use crate::error::ApiError;

pub async fn health() -> Json<Value> {
    Json(json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") }))
}

/// Placeholder for later phases. Takes `CurrentUser` so it can only ever be
/// mounted behind an authenticating guard.
pub async fn not_implemented(_user: CurrentUser) -> ApiError {
    ApiError::not_implemented()
}
