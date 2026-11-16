//! `/api/media`: a user's private uploads. The file itself lives under
//! `<data>/media/<owner>/<id>.<ext>` with its ffprobe sidecar (see
//! [`crate::media`]); these handlers spool the multipart upload, validate it
//! with ffprobe and answer with the asset and its reference count.

use std::path::PathBuf;

use axum::extract::multipart::MultipartError;
use axum::extract::{Multipart, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use ytp_auth::role::ADMIN;
use ytp_core::Error;

use crate::auth::{ClientIp, CurrentUser};
use crate::error::{ApiError, ApiPath, ApiQuery, ApiResult};
use crate::media::{Listed, Meter, Service, MAX_MEDIA_BYTES};
use crate::AppState;

/// Removes the spooled upload however the request ends.
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Whether the caller holds the `admin` role (admins see and delete every
/// user's assets; everybody else only their own).
fn is_admin(me: &CurrentUser) -> bool {
    me.user.roles.iter().any(|r| r == ADMIN)
}

/// A multipart failure: over the body limit is 413, anything else 400.
fn field_error(e: MultipartError) -> ApiError {
    if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
        ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            format!("the upload is over the {} MB limit", MAX_MEDIA_BYTES >> 20),
        )
    } else {
        ApiError::bad_request(e.body_text())
    }
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    /// `all=1`: every user's assets (admins only).
    #[serde(default)]
    pub all: Option<String>,
}

/// `GET /api/media`: your assets, newest first, each with how many stored
/// projects mention it. Admins may pass `?all=1`.
pub async fn list(
    State(st): State<AppState>,
    me: CurrentUser,
    ApiQuery(q): ApiQuery<ListQuery>,
) -> ApiResult<Json<Vec<Listed>>> {
    let service = Service::new(&st.config.data_dir);
    let all = matches!(q.all.as_deref(), Some("1" | "true" | "yes" | "on"));
    let assets = if all {
        if !is_admin(&me) {
            return Err(ApiError::forbidden("only an admin can list everyone's media"));
        }
        service.list_all()
    } else {
        service.list(me.user.id)
    };
    let ids: Vec<String> = assets.iter().map(|a| a.id.clone()).collect();
    let counts = st.auth.project_asset_counts(&ids).await?;
    let out = assets
        .into_iter()
        .map(|asset| Listed {
            referenced_by: counts.get(&asset.id).copied().unwrap_or(0),
            asset,
        })
        .collect();
    Ok(Json(out))
}

/// `POST /api/media`: multipart `file` (required) and optional `name`
/// (defaults to the file's stem). Over 200 MB is 413; a type outside the
/// allow-list, or a file ffprobe finds no stream in, is 422.
pub async fn upload(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    me: CurrentUser,
    mut form: Multipart,
) -> ApiResult<Json<Listed>> {
    let service = Service::new(&st.config.data_dir);
    let tmp = TempFile(service.upload_path());
    let mut filename: Option<String> = None;
    let mut name: Option<String> = None;

    while let Some(mut field) = form.next_field().await.map_err(field_error)? {
        match field.name() {
            Some("file") => {
                if filename.is_some() {
                    return Err(ApiError::bad_request("only one file per upload"));
                }
                filename = Some(field.file_name().unwrap_or("upload").to_string());
                let mut out = tokio::fs::File::create(&tmp.0).await.map_err(Error::from)?;
                let mut meter = Meter::new(MAX_MEDIA_BYTES);
                while let Some(chunk) = field.chunk().await.map_err(field_error)? {
                    if let Err(e) = meter.add(chunk.len()) {
                        return Err(ApiError::new(
                            StatusCode::PAYLOAD_TOO_LARGE,
                            "too_large",
                            e.to_string(),
                        ));
                    }
                    out.write_all(&chunk).await.map_err(Error::from)?;
                }
                out.flush().await.map_err(Error::from)?;
            }
            Some("name") => name = Some(field.text().await.map_err(field_error)?),
            _ => {}
        }
    }
    let Some(filename) = filename else {
        return Err(ApiError::bad_request("missing `file` field"));
    };

    // ffprobe runs a subprocess and the file is moved into place: off the
    // async runtime, like a render.
    let encoder = st.encoder.clone();
    let owner = me.user.id;
    let spool = tmp.0.clone();
    let (name, filename2) = (name.clone(), filename.clone());
    let asset = tokio::task::spawn_blocking(move || {
        service.install(owner, &spool, &filename2, name.as_deref(), &encoder)
    })
    .await
    .map_err(Error::internal)??;
    drop(tmp);

    st.auth
        .audit(
            Some(&me.user.username),
            "media.install",
            Some(&asset.id),
            Some(json!({
                "file": filename,
                "name": asset.name,
                "ext": asset.ext,
                "kind": asset.kind.as_str(),
                "bytes": asset.bytes,
                "duration": asset.duration,
            })),
            Some(&ip),
        )
        .await;
    Ok(Json(Listed {
        asset,
        referenced_by: 0,
    }))
}

/// `DELETE /api/media/:id`: your own asset (admins: anyone's). Unknown or
/// somebody else's is 404. A file a stored project still mentions is removed
/// too — like deleting a corpus, which also does not stop at references — and
/// the answer says how many projects now dangle in `X-Media-Referenced-By`.
pub async fn delete(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    me: CurrentUser,
    ApiPath(id): ApiPath<String>,
) -> ApiResult<Response> {
    let service = Service::new(&st.config.data_dir);
    let found = if is_admin(&me) {
        service.find_any(&id)
    } else {
        service.get(me.user.id, &id).map(|a| (me.user.id, a))
    };
    let Some((owner, _)) = found else {
        return Err(ApiError::not_found());
    };
    let ids = vec![id.clone()];
    let counts = st.auth.project_asset_counts(&ids).await?;
    let referenced_by = counts.get(&id).copied().unwrap_or(0);
    let asset = service.delete(owner, &id)?;

    st.auth
        .audit(
            Some(&me.user.username),
            "media.delete",
            Some(&asset.id),
            Some(json!({
                "owner": owner,
                "ext": asset.ext,
                "bytes": asset.bytes,
                "referenced_by": referenced_by,
            })),
            Some(&ip),
        )
        .await;

    let mut res = StatusCode::NO_CONTENT.into_response();
    if let Ok(v) = HeaderValue::from_str(&referenced_by.to_string()) {
        res.headers_mut()
            .insert(header::HeaderName::from_static("x-media-referenced-by"), v);
    }
    Ok(res)
}
