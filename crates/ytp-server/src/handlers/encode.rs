//! `GET /api/encode/presets`, `POST /api/jobs/:id/export`,
//! `GET /api/admin/encoders`: the HandBrake-style side of the API.

use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use ytp_auth::Permission;
use ytp_core::{Error, JobState};

use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiJson, ApiPath, ApiResult};
use crate::jobs::NewJob;
use crate::AppState;

/// Every preset an export may ask for, with the ones this caller may not use
/// marked (`allowed: false`, they need `export.hq`).
pub async fn presets(State(st): State<AppState>, me: CurrentUser) -> ApiResult<Json<Value>> {
    let hq = me.has(Permission::ExportHq);
    let presets: Vec<Value> = st
        .encoder
        .presets()
        .iter()
        .map(|p| {
            let mut v = p.to_json();
            v["allowed"] = json!(!p.hq || hq);
            v
        })
        .collect();
    Ok(Json(json!({ "hq": hq, "presets": presets })))
}

#[derive(Deserialize)]
pub struct ExportBody {
    pub preset: String,
}

/// Queue a re-encode of a finished job, from its kept master.
pub async fn export(
    State(st): State<AppState>,
    me: CurrentUser,
    ApiPath(id): ApiPath<String>,
    ApiJson(body): ApiJson<ExportBody>,
) -> ApiResult<Json<Value>> {
    let source = st.jobs.get(&id).ok_or_else(ApiError::not_found)?;
    if source.owner != me.user.id && !me.has(Permission::JobsViewAll) {
        return Err(ApiError::not_found());
    }
    if source.state != JobState::Done {
        return Err(Error::conflict("the job has no finished video to export").into());
    }
    let name = body.preset.trim();
    let Some(preset) = st.encoder.presets().get(name) else {
        return Err(Error::invalid(format!("no encode preset named {name:?}")).into());
    };
    if preset.hq && !me.has(Permission::ExportHq) {
        return Err(ApiError::forbidden("missing permission export.hq"));
    }
    if crate::encode::source_file(&st, &source).is_none() {
        return Err(Error::conflict("the render has no video file to encode").into());
    }
    let preset_name = preset.name.clone();
    let rec = st.jobs.submit_as(
        &me,
        NewJob {
            text: source.text.clone(),
            voice: source.voice.clone(),
            captions: false,
            options: json!({ "source_job": id, "preset": preset_name }),
        },
        Some("encode"),
    )?;
    Ok(Json(crate::jobs::accepted(&rec)))
}

/// The hardware encoders this machine's ffmpeg can really use (a test encode
/// each), and how many presets are loaded.
pub async fn encoders(State(st): State<AppState>, _me: CurrentUser) -> ApiResult<Json<Value>> {
    let encoder = st.encoder.clone();
    let report = tokio::task::spawn_blocking(move || encoder.hardware())
        .await
        .map_err(Error::internal)?;
    let mut v = serde_json::to_value(&*report).map_err(Error::internal)?;
    v["presets"] = json!(st.encoder.presets().len());
    Ok(Json(v))
}
