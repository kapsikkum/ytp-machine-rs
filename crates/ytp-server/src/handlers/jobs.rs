//! `POST /api/ytp/generate`, `GET|DELETE /api/jobs/:id`, `GET /api/jobs/:id/result`.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use tower::ServiceExt;
use tower_http::services::ServeFile;
use ytp_auth::Permission;
use ytp_core::{Error, JobState};

use crate::auth::{ClientIp, CurrentUser};
use crate::error::{ApiError, ApiJson, ApiPath, ApiResult};
use crate::jobs::{check_options, check_text, JobRecord, NewJob};
use crate::AppState;

#[derive(Deserialize)]
pub struct GenerateBody {
    pub text: String,
    /// Corpus to speak in; the first installed one when omitted.
    #[serde(default)]
    pub voice: Option<String>,
    #[serde(default)]
    pub captions: Option<bool>,
    #[serde(default)]
    pub options: Option<Value>,
}

pub async fn generate(
    State(st): State<AppState>,
    me: CurrentUser,
    ApiJson(body): ApiJson<GenerateBody>,
) -> ApiResult<Json<Value>> {
    let text = check_text(&body.text)?;
    let voice = match body.voice.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => {
            st.corpora
                .get(v)
                .await
                .map_err(|_| Error::invalid(format!("no corpus called {v:?} is installed")))?;
            v.to_string()
        }
        None => st
            .corpora
            .list()
            .await?
            .into_iter()
            .next()
            .map(|c| c.name)
            .ok_or_else(|| Error::invalid("no corpus is installed"))?,
    };
    let options = match body.options {
        None | Some(Value::Null) => json!({}),
        Some(v @ Value::Object(_)) => v,
        Some(_) => return Err(Error::invalid("options must be an object").into()),
    };
    check_options(&options)?;
    let rec = st.jobs.submit(
        &me,
        NewJob {
            text,
            voice,
            captions: body.captions.unwrap_or(true),
            options,
        },
    )?;
    Ok(Json(crate::jobs::accepted(&rec)))
}

/// Jobs are visible to their owner and to `jobs.view_all`; to anyone else
/// they do not exist. A token must carry some job-related permission.
fn visible(st: &AppState, me: &CurrentUser, id: &str) -> ApiResult<JobRecord> {
    let may = me.has(Permission::GenerateYtp)
        || me.has(Permission::GenerateYtpmv)
        || me.has(Permission::JobsViewAll);
    if !may {
        return Err(ApiError::forbidden("missing permission to see jobs"));
    }
    let rec = st.jobs.get(id).ok_or_else(ApiError::not_found)?;
    if rec.owner == me.user.id || me.has(Permission::JobsViewAll) {
        Ok(rec)
    } else {
        Err(ApiError::not_found())
    }
}

pub async fn get(
    State(st): State<AppState>,
    me: CurrentUser,
    ApiPath(id): ApiPath<String>,
) -> ApiResult<Json<JobRecord>> {
    Ok(Json(visible(&st, &me, &id)?))
}

/// Cancel an active job, or delete a finished one with its result.
pub async fn cancel(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    me: CurrentUser,
    ApiPath(id): ApiPath<String>,
) -> ApiResult<StatusCode> {
    let rec = visible(&st, &me, &id)?;
    let own = rec.owner == me.user.id;
    if !own && !me.has(Permission::JobsCancelAny) {
        return Err(ApiError::forbidden("only the owner can cancel this job"));
    }
    st.jobs.cancel(&id).ok_or_else(ApiError::not_found)?;
    if !own {
        st.auth
            .audit(
                Some(&me.user.username),
                "job.cancel",
                Some(&id),
                Some(json!({ "owner": rec.owner_name })),
                Some(&ip),
            )
            .await;
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn result(
    State(st): State<AppState>,
    me: CurrentUser,
    ApiPath(id): ApiPath<String>,
    req: Request<Body>,
) -> ApiResult<Response> {
    let rec = visible(&st, &me, &id)?;
    if rec.state != JobState::Done {
        return Err(Error::conflict("the job has no result").into());
    }
    let path = st.jobs.result_path(&id);
    if !path.is_file() {
        return Err(ApiError::not_found());
    }
    let mut res = ServeFile::new(path)
        .oneshot(req)
        .await
        .map_err(|e| ApiError::from(Error::internal(e)))?
        .into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp4"));
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("inline; filename=\"ytp-{id}.mp4\""))
            .unwrap_or_else(|_| HeaderValue::from_static("inline")),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=3600"));
    Ok(res)
}
