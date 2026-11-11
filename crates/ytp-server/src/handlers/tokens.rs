use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use ytp_auth::{Permission, Token};

use crate::auth::{ClientIp, CurrentUser};
use crate::error::{ApiJson, ApiPath, ApiResult};
use crate::AppState;

pub async fn list(State(st): State<AppState>, me: CurrentUser) -> ApiResult<Json<Vec<Token>>> {
    Ok(Json(st.auth.list_tokens(me.user.id).await?))
}

#[derive(Deserialize)]
pub struct NewToken {
    name: String,
    permissions: Vec<Permission>,
    expires_days: Option<u32>,
}

pub async fn create(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    me: CurrentUser,
    ApiJson(b): ApiJson<NewToken>,
) -> ApiResult<Json<Value>> {
    let (token, secret) = st
        .auth
        .create_token(&me, &b.name, &b.permissions, b.expires_days)
        .await?;
    st.auth
        .audit(
            Some(&me.user.username),
            "token.create",
            Some(&token.name),
            Some(json!({ "id": token.id, "permissions": token.permissions })),
            Some(&ip),
        )
        .await;
    Ok(Json(json!({ "token": token, "secret": secret })))
}

pub async fn delete(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    me: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    st.auth.delete_token(me.user.id, id).await?;
    st.auth
        .audit(
            Some(&me.user.username),
            "token.delete",
            Some(&id.to_string()),
            None,
            Some(&ip),
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}
