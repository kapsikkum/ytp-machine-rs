use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use ytp_auth::store::SESSION_TTL_SECS;
use ytp_auth::{role, secret, Permission, User};
use ytp_core::Error;

use crate::auth::{self as authn, ClientIp, CurrentUser, MaybeUser, SESSION_COOKIE};
use crate::error::{ApiError, ApiJson, ApiResult};
use crate::AppState;

pub async fn state(State(st): State<AppState>, MaybeUser(p): MaybeUser) -> ApiResult<Response> {
    let setup_required = st.setup_token.lock().await.is_some();
    let body = match p {
        Some(p) => json!({
            "setup_required": setup_required,
            "user": p.user,
            "permissions": p.permission_list(),
            "csrf": p.csrf(),
        }),
        None => json!({
            "setup_required": setup_required,
            "user": null,
            "permissions": [],
            "csrf": null,
        }),
    };
    Ok(Json(body).into_response())
}

/// Every permission id this server knows, for role/token editors.
pub async fn permissions(_u: CurrentUser) -> Json<&'static [Permission]> {
    Json(Permission::ALL)
}

fn user_agent(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
}

/// Start a fresh session for `user`, discarding any session the request
/// came in with (session ids rotate on every login).
async fn start_session(
    st: &AppState,
    headers: &HeaderMap,
    ip: &str,
    user: &User,
) -> ApiResult<(String, header::HeaderValue)> {
    if let Some(old) = authn::cookie(headers, SESSION_COOKIE) {
        st.auth.delete_session(old).await?;
    }
    let s = st
        .auth
        .create_session(user.id, Some(ip), user_agent(headers))
        .await?;
    let cookie = authn::session_cookie(st, headers, &s.secret, SESSION_TTL_SECS);
    Ok((s.csrf, cookie))
}

#[derive(Deserialize)]
pub struct SetupBody {
    token: String,
    username: String,
    password: String,
}

pub async fn setup(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    ApiJson(b): ApiJson<SetupBody>,
) -> ApiResult<Response> {
    // Held for the whole operation so two setups cannot both succeed.
    let mut slot = st.setup_token.lock().await;
    let Some(expected) = slot.as_deref() else {
        return Err(Error::conflict("setup has already been completed").into());
    };
    if !secret::ct_eq(b.token.trim(), expected) {
        st.auth
            .audit(None, "auth.setup_failed", None, None, Some(&ip))
            .await;
        return Err(ApiError::forbidden("invalid setup token"));
    }
    if st.auth.user_count().await? > 0 {
        *slot = None;
        return Err(Error::conflict("setup has already been completed").into());
    }
    let user = st
        .auth
        .create_user(b.username.trim(), &b.password, &[role::ADMIN.to_string()])
        .await?;
    *slot = None;
    drop(slot);
    st.auth
        .audit(
            Some(&user.username),
            "auth.setup",
            Some(&user.username),
            None,
            Some(&ip),
        )
        .await;
    tracing::info!(user = %user.username, "first-run setup completed");
    let (csrf, cookie) = start_session(&st, &headers, &ip, &user).await?;
    let (perms, _) = st.auth.grants(&user.roles).await?;
    Ok((
        [(header::SET_COOKIE, cookie)],
        Json(json!({ "user": user, "permissions": perms, "csrf": csrf })),
    )
        .into_response())
}

#[derive(Deserialize)]
pub struct LoginBody {
    username: String,
    password: String,
}

pub async fn login(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    ApiJson(b): ApiJson<LoginBody>,
) -> ApiResult<Response> {
    let username = b.username.trim();
    if !st.limiter.attempt(&ip, username) {
        st.auth
            .audit(
                None,
                "auth.login_rate_limited",
                Some(username),
                None,
                Some(&ip),
            )
            .await;
        return Err(Error::RateLimited.into());
    }
    let Some(user) = st.auth.verify_login(username, &b.password).await? else {
        st.auth
            .audit(None, "auth.login_failed", Some(username), None, Some(&ip))
            .await;
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_credentials",
            "wrong username or password",
        ));
    };
    let (csrf, cookie) = start_session(&st, &headers, &ip, &user).await?;
    let (perms, _) = st.auth.grants(&user.roles).await?;
    st.auth
        .audit(Some(&user.username), "auth.login", None, None, Some(&ip))
        .await;
    Ok((
        [(header::SET_COOKIE, cookie)],
        Json(json!({
            "user": user,
            "permissions": perms,
            "csrf": csrf,
        })),
    )
        .into_response())
}

pub async fn logout(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    user: CurrentUser,
) -> ApiResult<Response> {
    if let Some(h) = user.session_hash() {
        st.auth.delete_session_hash(h).await?;
    }
    st.auth
        .audit(
            Some(&user.user.username),
            "auth.logout",
            None,
            None,
            Some(&ip),
        )
        .await;
    Ok((
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, authn::clear_cookie(&st, &headers))],
    )
        .into_response())
}

#[derive(Deserialize)]
pub struct PasswordBody {
    current: String,
    new: String,
}

pub async fn change_password(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    user: CurrentUser,
    ApiJson(b): ApiJson<PasswordBody>,
) -> ApiResult<StatusCode> {
    st.auth
        .change_password(user.user.id, &b.current, &b.new, user.session_hash())
        .await?;
    st.auth
        .audit(
            Some(&user.user.username),
            "auth.password",
            None,
            None,
            Some(&ip),
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}
