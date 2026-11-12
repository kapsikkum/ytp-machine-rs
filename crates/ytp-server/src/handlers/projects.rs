//! `/api/projects` (the Studio's documents, with owners and shares),
//! `/api/ytp/plan`, `/api/ytpmv/plan` (a generator's plan as a project, no
//! rendering) and `/api/preview` (a low-res render of a time window).

use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use ytp_auth::{Access, Permission, StoredProject};
use ytp_core::{Error, Project};

use crate::auth::{ClientIp, CurrentUser};
use crate::error::{ApiError, ApiJson, ApiPath, ApiResult};
use crate::jobs::{check_options, check_text, NewJob};
use crate::projects::{self, kind_name, parse_project, SongRequest, MAX_PROJECT_BYTES};
use crate::ytpmv::ascii_json;
use crate::AppState;

const MAX_PARTS: usize = 256;

/// A project the caller may open; anything else does not exist for them.
async fn open(st: &AppState, me: &CurrentUser, id: i64) -> ApiResult<StoredProject> {
    st.auth.project_get(id, me.user.id).await?.ok_or_else(ApiError::not_found)
}

fn need_write(p: &StoredProject) -> ApiResult<()> {
    if p.meta.access.can_write() {
        Ok(())
    } else {
        Err(ApiError::forbidden("this project is shared with you read-only"))
    }
}

fn need_owner(p: &StoredProject, what: &str) -> ApiResult<()> {
    if p.meta.access == Access::Owner {
        Ok(())
    } else {
        Err(ApiError::forbidden(format!("only the owner can {what}")))
    }
}

/// The project as the API shows it: the document with its id and owner filled in.
fn view(p: &StoredProject) -> ApiResult<Value> {
    let mut doc: Project = serde_json::from_str(&p.data).map_err(|e| Error::internal(format!("stored project: {e}")))?;
    doc.id = Some(p.meta.id);
    doc.owner = Some(p.meta.owner);
    let mut v = serde_json::to_value(&p.meta).map_err(Error::internal)?;
    v["project"] = serde_json::to_value(&doc).map_err(Error::internal)?;
    Ok(v)
}

/// Serialise a project for storage (no id or owner inside: the row has them).
fn document(p: &Project) -> ApiResult<String> {
    let mut doc = p.clone();
    doc.id = None;
    doc.owner = None;
    let text = serde_json::to_string(&doc).map_err(Error::internal)?;
    if text.len() > MAX_PROJECT_BYTES {
        return Err(Error::invalid(format!("the project is {} MB; the limit is {} MB", text.len() >> 20, MAX_PROJECT_BYTES >> 20)).into());
    }
    Ok(text)
}

async fn store_new(st: &AppState, me: &CurrentUser, mut project: Project, name: Option<&str>) -> ApiResult<Value> {
    if let Some(n) = name.map(str::trim).filter(|n| !n.is_empty()) {
        project.name = n.to_string();
    }
    project.validate().map_err(Error::invalid)?;
    let data = document(&project)?;
    let id = st
        .auth
        .project_create(me.user.id, project.name.trim(), &data, kind_name(&project), project.duration(), project.clip_count() as i64)
        .await?;
    view(&open(st, me, id).await?)
}

pub async fn list(State(st): State<AppState>, me: CurrentUser) -> ApiResult<Json<Value>> {
    Ok(Json(serde_json::to_value(st.auth.project_list(me.user.id).await?).map_err(Error::internal)?))
}

#[derive(Deserialize)]
pub struct CreateBody {
    pub project: Value,
    #[serde(default)]
    pub name: Option<String>,
}

pub async fn create(State(st): State<AppState>, me: CurrentUser, ApiJson(body): ApiJson<CreateBody>) -> ApiResult<Json<Value>> {
    let project = parse_project(body.project)?;
    Ok(Json(store_new(&st, &me, project, body.name.as_deref()).await?))
}

pub async fn get(State(st): State<AppState>, me: CurrentUser, ApiPath(id): ApiPath<i64>) -> ApiResult<Json<Value>> {
    Ok(Json(view(&open(&st, &me, id).await?)?))
}

#[derive(Deserialize)]
pub struct UpdateBody {
    #[serde(default)]
    pub project: Option<Value>,
    #[serde(default)]
    pub name: Option<String>,
    /// The revision the edit was made on; a different stored one is a 409.
    #[serde(default)]
    pub revision: Option<i64>,
}

pub async fn update(
    State(st): State<AppState>,
    me: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(body): ApiJson<UpdateBody>,
) -> ApiResult<Json<Value>> {
    let stored = open(&st, &me, id).await?;
    need_write(&stored)?;
    if body.project.is_none() && body.name.is_none() {
        return Err(Error::invalid("send a `project` or a `name`").into());
    }
    let mut project = match body.project {
        Some(v) => parse_project(v)?,
        None => serde_json::from_str::<Project>(&stored.data).map_err(|e| Error::internal(format!("stored project: {e}")))?,
    };
    if let Some(n) = body.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        project.name = n.to_string();
    }
    project.validate().map_err(Error::invalid)?;
    let data = document(&project)?;
    st.auth
        .project_update(id, project.name.trim(), &data, kind_name(&project), project.duration(), project.clip_count() as i64, body.revision)
        .await?;
    Ok(Json(view(&open(&st, &me, id).await?)?))
}

pub async fn delete(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    me: CurrentUser,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<StatusCode> {
    let stored = open(&st, &me, id).await?;
    need_owner(&stored, "delete a project")?;
    st.auth.project_delete(id).await?;
    st.auth
        .audit(Some(&me.user.username), "project.delete", Some(&id.to_string()), Some(json!({ "name": stored.meta.name })), Some(&ip))
        .await;
    Ok(StatusCode::NO_CONTENT)
}

// ── Sharing ──────────────────────────────────────────────────────────────────

pub async fn shares(State(st): State<AppState>, me: CurrentUser, ApiPath(id): ApiPath<i64>) -> ApiResult<Json<Value>> {
    let stored = open(&st, &me, id).await?;
    need_owner(&stored, "see who a project is shared with")?;
    Ok(Json(serde_json::to_value(st.auth.project_shares(id).await?).map_err(Error::internal)?))
}

#[derive(Deserialize)]
pub struct ShareBody {
    pub username: String,
    /// `read` or `write`.
    pub access: String,
}

pub async fn share(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    me: CurrentUser,
    ApiPath(id): ApiPath<i64>,
    ApiJson(body): ApiJson<ShareBody>,
) -> ApiResult<Json<Value>> {
    let stored = open(&st, &me, id).await?;
    need_owner(&stored, "share a project")?;
    let access = match body.access.as_str() {
        "read" => Access::Read,
        "write" => Access::Write,
        _ => return Err(Error::invalid("`access` is \"read\" or \"write\"").into()),
    };
    let target = st
        .auth
        .find_user(body.username.trim())
        .await?
        .filter(|u| !u.disabled)
        .ok_or_else(|| Error::invalid(format!("no user called {:?}", body.username.trim())))?;
    if target.id == me.user.id {
        return Err(Error::invalid("the project is already yours").into());
    }
    st.auth.project_share(id, target.id, access).await?;
    st.auth
        .audit(
            Some(&me.user.username),
            "project.share",
            Some(&id.to_string()),
            Some(json!({ "with": target.username, "access": access.as_str() })),
            Some(&ip),
        )
        .await;
    Ok(Json(serde_json::to_value(st.auth.project_shares(id).await?).map_err(Error::internal)?))
}

pub async fn unshare(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    me: CurrentUser,
    ApiPath((id, username)): ApiPath<(i64, String)>,
) -> ApiResult<StatusCode> {
    let stored = open(&st, &me, id).await?;
    need_owner(&stored, "change who a project is shared with")?;
    let target = st.auth.find_user(&username).await?.ok_or_else(ApiError::not_found)?;
    st.auth.project_unshare(id, target.id).await?;
    st.auth
        .audit(Some(&me.user.username), "project.unshare", Some(&id.to_string()), Some(json!({ "with": target.username })), Some(&ip))
        .await;
    Ok(StatusCode::NO_CONTENT)
}

// ── Rendering ────────────────────────────────────────────────────────────────

/// Queue a full-quality render of a project; follow it as a job.
pub async fn render(State(st): State<AppState>, me: CurrentUser, ApiPath(id): ApiPath<i64>) -> ApiResult<Json<Value>> {
    let stored = open(&st, &me, id).await?;
    let project: Project = serde_json::from_str(&stored.data).map_err(|e| Error::internal(format!("stored project: {e}")))?;
    if project.duration() <= 0.0 {
        return Err(Error::invalid("the project has no clips to render").into());
    }
    let rec = st.jobs.submit_as(
        &me,
        NewJob { text: stored.meta.name.clone(), voice: project.corpus.clone(), captions: project.captions, options: json!({ "project_id": id }) },
        Some("project"),
    )?;
    Ok(Json(crate::jobs::accepted(&rec)))
}

// ── Plans ────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct PlanBody {
    pub text: String,
    #[serde(default)]
    pub voice: Option<String>,
    #[serde(default)]
    pub captions: Option<bool>,
    #[serde(default)]
    pub options: Option<Value>,
    /// Store the plan as a new project of the caller's and answer with that.
    #[serde(default)]
    pub save: bool,
    #[serde(default)]
    pub name: Option<String>,
}

/// A sentence plan as a project: what `/api/ytp/generate` would say, without rendering.
pub async fn plan_ytp(State(st): State<AppState>, me: CurrentUser, ApiJson(body): ApiJson<PlanBody>) -> ApiResult<Json<Value>> {
    let text = check_text(&body.text)?;
    let voice = match body.voice.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => {
            st.corpora.get(v).await.map_err(|_| Error::invalid(format!("no corpus called {v:?} installed")))?;
            v.to_string()
        }
        None => st
            .corpora
            .list()
            .await?
            .into_iter()
            .next()
            .map(|c| c.name)
            .ok_or_else(|| Error::invalid("no corpus installed"))?,
    };
    let options = match body.options {
        None | Some(Value::Null) => json!({}),
        Some(v @ Value::Object(_)) => v,
        Some(_) => return Err(Error::invalid("options must be an object").into()),
    };
    check_options(&options)?;
    let (project, report) = projects::plan_ytp(&st, text, voice, body.captions.unwrap_or(true), options).await?;
    if body.save {
        if !me.has(Permission::ProjectWrite) {
            return Err(ApiError::forbidden("missing permission project.write"));
        }
        let saved = store_new(&st, &me, project, body.name.as_deref()).await?;
        return Ok(Json(json!({ "id": saved["id"], "saved": saved, "report": report })));
    }
    Ok(Json(json!({ "project": project, "report": report })))
}

fn str_field<'a>(body: &'a Value, key: &str) -> ApiResult<&'a str> {
    body.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid(format!("`{key}` is required")).into())
}

/// A song plan as a project: one track per part, one clip per note. Takes
/// the body of `/api/ytpmv/render`, plus `save` and `name`.
pub async fn plan_ytpmv(State(st): State<AppState>, me: CurrentUser, ApiJson(body): ApiJson<Value>) -> ApiResult<Json<Value>> {
    let midi_id = str_field(&body, "midi_id")?.to_string();
    let parts: Vec<Value> = match body.get("parts") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) if a.len() <= MAX_PARTS => a.clone(),
        Some(_) => return Err(Error::invalid(format!("`parts` must be a list of at most {MAX_PARTS}")).into()),
    };
    let options = match body.get("options") {
        None | Some(Value::Null) => json!({}),
        Some(v @ Value::Object(_)) => v.clone(),
        Some(_) => return Err(Error::invalid("`options` must be an object").into()),
    };
    let synth = ytp_ytpmv::settings::truthy(options.get("synth"));
    let voice = match body.get("voice").and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => {
            st.corpora.get(v).await.map_err(|_| Error::invalid(format!("The corpus '{v}' is not installed. Choose another voice.")))?;
            v.to_string()
        }
        None => st.corpora.list().await?.into_iter().next().map(|c| c.name).unwrap_or_default(),
    };
    if voice.is_empty() && !synth {
        return Err(Error::invalid("no corpus is installed").into());
    }
    let project = projects::plan_ytpmv(&st, me.user.id, SongRequest { midi_id, parts, options, voice }, me.quota.max_height).await?;
    if body.get("save").and_then(Value::as_bool).unwrap_or(false) {
        if !me.has(Permission::ProjectWrite) {
            return Err(ApiError::forbidden("missing permission project.write"));
        }
        let name = body.get("name").and_then(Value::as_str);
        let saved = store_new(&st, &me, project, name).await?;
        return Ok(Json(json!({ "id": saved["id"], "saved": saved })));
    }
    Ok(Json(json!({ "project": project })))
}

// ── Preview ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct PreviewBody {
    /// A stored project...
    #[serde(default)]
    pub project_id: Option<i64>,
    /// ...or one sent as it is being edited.
    #[serde(default)]
    pub project: Option<Value>,
    #[serde(default)]
    pub start: Option<f64>,
    #[serde(default)]
    pub duration: Option<f64>,
    /// Mixer changes made since the project was stored (mute, solo, volume per
    /// track id), so the preview follows the faders without sending the project.
    #[serde(default)]
    pub tracks: Vec<TrackMix>,
}

#[derive(Deserialize)]
pub struct TrackMix {
    pub id: String,
    #[serde(default)]
    pub mute: Option<bool>,
    #[serde(default)]
    pub solo: Option<bool>,
    #[serde(default)]
    pub volume: Option<f64>,
}

/// `start..start + duration` of a project as a small MP4 (`video/mp4`);
/// what happened is in `X-Preview-Info`.
pub async fn preview(State(st): State<AppState>, me: CurrentUser, ApiJson(body): ApiJson<PreviewBody>) -> ApiResult<Response> {
    let (mut project, owner) = match (body.project_id, body.project) {
        (Some(id), None) => {
            if !me.has(Permission::ProjectRead) {
                return Err(ApiError::forbidden("missing permission project.read"));
            }
            let stored = open(&st, &me, id).await?;
            let project: Project = serde_json::from_str(&stored.data).map_err(|e| Error::internal(format!("stored project: {e}")))?;
            (project, stored.meta.owner)
        }
        (None, Some(v)) => (parse_project(v)?, me.user.id),
        _ => return Err(Error::invalid("send either `project_id` or `project`").into()),
    };
    for m in &body.tracks {
        let Some(t) = project.tracks.iter_mut().find(|t| t.id == m.id) else {
            return Err(Error::invalid(format!("no track {:?}", m.id)).into());
        };
        t.mute = m.mute.unwrap_or(t.mute);
        t.solo = m.solo.unwrap_or(t.solo);
        t.volume = m.volume.unwrap_or(t.volume);
    }
    project.validate().map_err(Error::invalid)?;
    let start = body.start.unwrap_or(0.0);
    let duration = body.duration.unwrap_or(10.0);
    let result = projects::preview(&st, project, owner, start, duration).await?;
    let bytes = tokio::fs::read(&result.path).await.map_err(Error::internal)?;
    let mut res = (StatusCode::OK, bytes).into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp4"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert("x-preview-cache", HeaderValue::from_static(if result.cached { "hit" } else { "miss" }));
    h.insert("x-preview-ms", HeaderValue::from(result.millis as u64));
    if let Ok(v) = HeaderValue::from_str(&ascii_json(&result.info)) {
        h.insert("x-preview-info", v);
    }
    Ok(res)
}
