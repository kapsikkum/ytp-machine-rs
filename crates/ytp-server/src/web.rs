//! The embedded SvelteKit build, served with an SPA fallback.

use std::borrow::Cow;

use axum::http::{header, Method, StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use rust_embed::RustEmbed;

use crate::error::ApiError;

#[derive(RustEmbed)]
#[folder = "$YTP_WEB_DIR"]
struct Assets;

fn serve(path: &str, file: rust_embed::EmbeddedFile) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    // SvelteKit fingerprints everything under _app/immutable.
    let cache = if path.starts_with("_app/immutable/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (
        [
            (header::CONTENT_TYPE, mime.as_ref().to_string()),
            (header::CACHE_CONTROL, cache.to_string()),
        ],
        file.data,
    )
        .into_response()
}

pub fn has_ui() -> bool {
    Assets::get("index.html").is_some() || Assets::get("200.html").is_some()
}

pub async fn static_or_spa(method: Method, uri: Uri) -> Response {
    let path = uri.path();
    if path == "/api" || path.starts_with("/api/") {
        return ApiError::not_found().into_response();
    }
    if method != Method::GET && method != Method::HEAD {
        return ApiError::not_found().into_response();
    }
    let rel = path.trim_start_matches('/');
    if rel.split('/').any(|p| p == ".." || p.contains('\\')) {
        return ApiError::not_found().into_response();
    }
    let candidates: Vec<Cow<str>> = if rel.is_empty() {
        vec!["index.html".into()]
    } else {
        let trimmed = rel.trim_end_matches('/');
        vec![
            rel.into(),
            format!("{trimmed}/index.html").into(),
            format!("{trimmed}.html").into(),
        ]
    };
    for c in &candidates {
        if let Some(f) = Assets::get(c) {
            return serve(c, f);
        }
    }
    // A missing hashed asset is a real 404, not a page.
    if rel.starts_with("_app/") {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    for fallback in ["index.html", "200.html"] {
        if let Some(f) = Assets::get(fallback) {
            return serve(fallback, f);
        }
    }
    (
        StatusCode::NOT_FOUND,
        Html("<!doctype html><title>ytp-machine</title><p>The web UI has not been built. Run <code>npm run build</code> in <code>web/</code>, then rebuild the server.</p>"),
    )
        .into_response()
}
