//! HTTP server for ytp-machine: JSON API under `/api`, WebSocket events at
//! `/api/ws`, and the embedded SvelteKit UI for everything else.

pub mod auth;
pub mod config;
pub mod error;
pub mod handlers;
pub mod jobs;
pub mod routes;
pub mod state;
pub mod throttle;
pub mod web;
pub mod ws;
pub mod ytpmv;

use axum::http::{header, HeaderValue};
use axum::Router;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

pub use config::Config;
pub use state::AppState;

/// Build the full application router.
pub fn app(state: AppState) -> Router {
    let (api, _specs) = routes::api(&state);
    api.fallback(web::static_or_spa)
        .with_state(state)
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        // The setup URL carries a secret; never leak it via Referer.
        .layer(SetResponseHeaderLayer::if_not_present(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(TraceLayer::new_for_http())
}
