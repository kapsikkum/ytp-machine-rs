//! Request authentication: who is calling, may they call this route, and is
//! the request forged (CSRF / foreign WebSocket origin).

use std::net::SocketAddr;
use std::ops::Deref;

use axum::extract::{ConnectInfo, FromRequestParts, Request};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, HeaderValue, Method};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use ytp_auth::{secret, Credential, Permission, Principal};

use crate::error::{ApiError, ApiResult};
use crate::routes::Access;
use crate::throttle::Throttled;
use crate::AppState;

pub const SESSION_COOKIE: &str = "ytp_session";
pub const CSRF_HEADER: &str = "x-csrf-token";

/// The authenticated caller. Only available on routes whose `Access`
/// requires authentication; the guard has already checked permissions.
#[derive(Debug, Clone)]
pub struct CurrentUser(pub Principal);

impl Deref for CurrentUser {
    type Target = Principal;

    fn deref(&self) -> &Principal {
        &self.0
    }
}

impl<S: Send + Sync> FromRequestParts<S> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, ApiError> {
        parts
            .extensions
            .get::<CurrentUser>()
            .cloned()
            .ok_or_else(ApiError::unauthorized)
    }
}

/// The caller if there is one (`Access::Optional` routes).
#[derive(Debug, Clone)]
pub struct MaybeUser(pub Option<Principal>);

impl<S: Send + Sync> FromRequestParts<S> for MaybeUser {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(parts
            .extensions
            .get::<MaybeUser>()
            .cloned()
            .unwrap_or(MaybeUser(None)))
    }
}

/// Explicit in-handler check, for decisions finer than the route's access.
pub fn require(user: &Principal, p: Permission) -> ApiResult<()> {
    user.require(p).map_err(Into::into)
}

/// Client address as a string, for rate limiting and the audit log.
#[derive(Debug, Clone)]
pub struct ClientIp(pub String);

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        Ok(ClientIp(client_ip(st, &parts.headers, &parts.extensions)))
    }
}

pub fn client_ip(st: &AppState, headers: &HeaderMap, ext: &axum::http::Extensions) -> String {
    if st.config.trust_proxy {
        if let Some(first) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
        {
            let ip = first.trim();
            if !ip.is_empty() {
                return ip.to_string();
            }
        }
    }
    ext.get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip().to_string())
        .unwrap_or_else(|| "unknown".into())
}

pub fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.trim())
        .filter(|v| !v.is_empty())
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, rest) = v.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| rest.trim())
}

fn query_token(uri: &axum::http::Uri) -> Option<String> {
    uri.query()?
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == "token")
        .map(|(_, v)| v.to_string())
}

fn is_https(st: &AppState, headers: &HeaderMap) -> bool {
    st.config.secure_cookies
        || headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("https"))
}

pub fn session_cookie(
    st: &AppState,
    headers: &HeaderMap,
    value: &str,
    max_age: i64,
) -> HeaderValue {
    let secure = if is_https(st, headers) {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}"
    ))
    .expect("cookie is ascii")
}

pub fn clear_cookie(st: &AppState, headers: &HeaderMap) -> HeaderValue {
    session_cookie(st, headers, "", 0)
}

/// Identify the caller. A bearer token that does not resolve is an error
/// (the client clearly meant to authenticate); a stale cookie is just
/// "nobody".
pub async fn resolve(
    st: &AppState,
    headers: &HeaderMap,
    uri: &axum::http::Uri,
    allow_query_token: bool,
) -> ApiResult<Option<Principal>> {
    let token = bearer(headers)
        .map(str::to_string)
        .or_else(|| allow_query_token.then(|| query_token(uri)).flatten());
    if let Some(t) = token {
        return match st.auth.token_principal(&t).await? {
            Some(p) => Ok(Some(p)),
            None => Err(ApiError::new(
                axum::http::StatusCode::UNAUTHORIZED,
                "unauthorized",
                "invalid or expired token",
            )),
        };
    }
    if let Some(c) = cookie(headers, SESSION_COOKIE) {
        return Ok(st.auth.session_principal(c).await?);
    }
    Ok(None)
}

/// Requests that start generation work count against `requests_per_minute`.
fn is_throttled(m: &Method, path: &str) -> bool {
    if is_safe_method(m) {
        return false;
    }
    // Uploading, renaming and deleting a MIDI file is cheap; only the routes
    // that make audio or video count.
    path == "/api/ytp/generate"
        || matches!(
            path,
            "/api/ytpmv/sample" | "/api/ytpmv/sample/audio" | "/api/ytpmv/preview" | "/api/ytpmv/preview-mix" | "/api/ytpmv/render"
        )
}

fn is_safe_method(m: &Method) -> bool {
    matches!(*m, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// Per-route middleware: every API route goes through this with its declared
/// `Access`, so no handler runs without its permission having been checked.
pub async fn guard(st: AppState, access: Access, mut req: Request, next: Next) -> Response {
    match check(&st, access, &mut req).await {
        Ok(()) => next.run(req).await,
        Err(e) => e.into_response(),
    }
}

async fn check(st: &AppState, access: Access, req: &mut Request) -> ApiResult<()> {
    let ws = req.uri().path() == "/api/ws";
    match access {
        Access::Public | Access::SetupToken => return Ok(()),
        Access::Optional => {
            let p = resolve(st, req.headers(), req.uri(), false)
                .await
                .ok()
                .flatten();
            req.extensions_mut().insert(MaybeUser(p));
            return Ok(());
        }
        _ => {}
    }
    let p = resolve(st, req.headers(), req.uri(), ws)
        .await?
        .ok_or_else(ApiError::unauthorized)?;

    if let Credential::Session { csrf, .. } = &p.credential {
        if !is_safe_method(req.method()) {
            let sent = req
                .headers()
                .get(CSRF_HEADER)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if !secret::ct_eq(sent, csrf) {
                return Err(ApiError::new(
                    axum::http::StatusCode::FORBIDDEN,
                    "csrf_failed",
                    "missing or wrong X-CSRF-Token header",
                ));
            }
        }
    }

    match access {
        Access::Session if p.session_hash().is_none() => {
            return Err(ApiError::forbidden(
                "this needs a browser session, not a token",
            ))
        }
        Access::Perm(perm) => require(&p, perm)?,
        Access::ReadWrite { read, write } => {
            let perm = if is_safe_method(req.method()) {
                read
            } else {
                write
            };
            require(&p, perm)?
        }
        _ => {}
    }

    // After the permission check: a request that would be refused anyway
    // does not spend the caller's tokens.
    if is_throttled(req.method(), req.uri().path()) {
        if let Err(t) = st.throttle.take(p.user.id, p.quota.requests_per_minute) {
            return Err(match t {
                Throttled::Off => ApiError::forbidden("your role may not make these requests"),
                Throttled::Wait(secs) => ApiError::rate_limited(secs),
            });
        }
    }
    req.extensions_mut().insert(CurrentUser(p));
    Ok(())
}

/// Browsers always send `Origin` on WebSocket handshakes; it must be this
/// server (same host) or an explicitly allowed origin. Non-browser clients
/// that send none are let through (they authenticate like anyone else).
pub fn origin_allowed(st: &AppState, headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    if st
        .config
        .allowed_origins
        .iter()
        .any(|a| a.trim_end_matches('/').eq_ignore_ascii_case(origin))
    {
        return true;
    }
    let Some((_, host_part)) = origin.split_once("://") else {
        return false;
    };
    let mut hosts: Vec<&str> = Vec::new();
    if let Some(h) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) {
        hosts.push(h);
    }
    if st.config.trust_proxy {
        if let Some(h) = headers
            .get("x-forwarded-host")
            .and_then(|v| v.to_str().ok())
        {
            hosts.push(h);
        }
    }
    hosts.iter().any(|h| h.eq_ignore_ascii_case(host_part))
}
