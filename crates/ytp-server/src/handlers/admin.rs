use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use ytp_auth::{AuditEntry, Permission, Quota, Role, User};
use ytp_core::Error;

use crate::auth::{ClientIp, CurrentUser};
use crate::error::{ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::AppState;

pub async fn list_users(State(st): State<AppState>, _u: CurrentUser) -> ApiResult<Json<Vec<User>>> {
    Ok(Json(st.auth.list_users().await?))
}

#[derive(Deserialize)]
pub struct NewUser {
    username: String,
    password: String,
    #[serde(default)]
    roles: Vec<String>,
}

pub async fn create_user(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    me: CurrentUser,
    ApiJson(b): ApiJson<NewUser>,
) -> ApiResult<Json<User>> {
    let user = st
        .auth
        .create_user(b.username.trim(), &b.password, &b.roles)
        .await?;
    st.auth
        .audit(
            Some(&me.user.username),
            "user.create",
            Some(&user.username),
            Some(json!({ "roles": user.roles })),
            Some(&ip),
        )
        .await;
    Ok(Json(user))
}

#[derive(Deserialize)]
pub struct UserPatch {
    roles: Option<Vec<String>>,
    disabled: Option<bool>,
    password: Option<String>,
}

pub async fn update_user(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    me: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(b): ApiJson<UserPatch>,
) -> ApiResult<Json<User>> {
    let detail = json!({
        "roles": b.roles,
        "disabled": b.disabled,
        "password_changed": b.password.is_some(),
    });
    let user = st
        .auth
        .update_user(id, b.roles, b.disabled, b.password)
        .await?;
    st.auth
        .audit(
            Some(&me.user.username),
            "user.update",
            Some(&user.username),
            Some(detail),
            Some(&ip),
        )
        .await;
    Ok(Json(user))
}

pub async fn delete_user(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    me: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    if id == me.user.id {
        return Err(Error::conflict("you cannot delete your own account").into());
    }
    let user = st.auth.delete_user(id).await?;
    st.auth
        .audit(
            Some(&me.user.username),
            "user.delete",
            Some(&user.username),
            None,
            Some(&ip),
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_roles(State(st): State<AppState>, _u: CurrentUser) -> ApiResult<Json<Vec<Role>>> {
    Ok(Json(st.auth.list_roles().await?))
}

#[derive(Deserialize)]
pub struct NewRole {
    name: String,
    permissions: Vec<Permission>,
    quota: Option<Quota>,
}

pub async fn create_role(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    me: CurrentUser,
    ApiJson(b): ApiJson<NewRole>,
) -> ApiResult<Json<Role>> {
    let role = st
        .auth
        .create_role(b.name.trim(), &b.permissions, b.quota)
        .await?;
    st.auth
        .audit(
            Some(&me.user.username),
            "role.create",
            Some(&role.name),
            Some(json!({ "permissions": role.permissions, "quota": role.quota })),
            Some(&ip),
        )
        .await;
    Ok(Json(role))
}

#[derive(Deserialize)]
pub struct RolePatch {
    permissions: Option<Vec<Permission>>,
    quota: Option<Quota>,
}

pub async fn update_role(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    me: CurrentUser,
    ApiPath(name): ApiPath<String>,
    ApiJson(b): ApiJson<RolePatch>,
) -> ApiResult<Json<Role>> {
    let role = st
        .auth
        .update_role(&name, b.permissions.as_deref(), b.quota)
        .await?;
    st.auth
        .audit(
            Some(&me.user.username),
            "role.update",
            Some(&role.name),
            Some(json!({ "permissions": b.permissions, "quota": b.quota })),
            Some(&ip),
        )
        .await;
    Ok(Json(role))
}

pub async fn delete_role(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    me: CurrentUser,
    ApiPath(name): ApiPath<String>,
) -> ApiResult<StatusCode> {
    st.auth.delete_role(&name).await?;
    st.auth
        .audit(
            Some(&me.user.username),
            "role.delete",
            Some(&name),
            None,
            Some(&ip),
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct AuditQuery {
    limit: Option<i64>,
    before: Option<i64>,
}

pub async fn audit(
    State(st): State<AppState>,
    _u: CurrentUser,
    ApiQuery(q): ApiQuery<AuditQuery>,
) -> ApiResult<Json<Vec<AuditEntry>>> {
    Ok(Json(
        st.auth.list_audit(q.limit.unwrap_or(100), q.before).await?,
    ))
}
