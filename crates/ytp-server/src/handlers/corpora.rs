use std::path::PathBuf;

use axum::extract::{Multipart, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use ytp_core::{CorpusEvent, Error, Event};
use ytp_corpus::{slug_from_filename, Corpus};

use crate::auth::{ClientIp, CurrentUser};
use crate::error::{ApiError, ApiPath, ApiResult};
use crate::routes::MAX_BUNDLE_BYTES;
use crate::AppState;

pub async fn list(State(st): State<AppState>, _u: CurrentUser) -> ApiResult<Json<Vec<Corpus>>> {
    Ok(Json(st.corpora.list().await?))
}

/// Removes the spooled upload however the request ends.
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Multipart: `file` (required, the `.tar.zst`), optional `name` (defaults to
/// the file name) and `replace` (`true` to overwrite an existing corpus).
pub async fn install(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    me: CurrentUser,
    mut form: Multipart,
) -> ApiResult<Json<Corpus>> {
    let bad = |e: axum::extract::multipart::MultipartError| ApiError::bad_request(e.body_text());
    let tmp = TempFile(st.corpora.upload_path());
    let mut filename: Option<String> = None;
    let mut name: Option<String> = None;
    let mut replace = false;

    while let Some(mut field) = form.next_field().await.map_err(bad)? {
        match field.name() {
            Some("file") => {
                if filename.is_some() {
                    return Err(ApiError::bad_request("only one file per install"));
                }
                filename = Some(field.file_name().unwrap_or("corpus.tar.zst").to_string());
                let mut out = tokio::fs::File::create(&tmp.0).await.map_err(Error::from)?;
                let mut size = 0usize;
                while let Some(chunk) = field.chunk().await.map_err(bad)? {
                    size += chunk.len();
                    if size > MAX_BUNDLE_BYTES {
                        return Err(Error::TooLarge("bundle is over 8 GiB".into()).into());
                    }
                    out.write_all(&chunk).await.map_err(Error::from)?;
                }
                out.flush().await.map_err(Error::from)?;
            }
            Some("name") => name = Some(field.text().await.map_err(bad)?.trim().to_string()),
            Some("replace") => {
                let v = field.text().await.map_err(bad)?;
                replace = matches!(v.trim(), "true" | "1" | "on" | "yes");
            }
            _ => {}
        }
    }
    let Some(filename) = filename else {
        return Err(ApiError::bad_request("missing `file` field"));
    };
    let name = name
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| slug_from_filename(&filename));

    let corpus = st.corpora.install(&tmp.0, &name, replace).await?;
    drop(tmp);
    st.auth
        .audit(
            Some(&me.user.username),
            "corpus.install",
            Some(&corpus.name),
            Some(json!({ "file": filename, "replace": replace, "clips": corpus.clips })),
            Some(&ip),
        )
        .await;
    st.publish(Event::Corpus {
        event: CorpusEvent::Installed,
        name: corpus.name.clone(),
    });
    Ok(Json(corpus))
}

pub async fn delete(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    me: CurrentUser,
    ApiPath(name): ApiPath<String>,
) -> ApiResult<StatusCode> {
    st.corpora.delete(&name).await?;
    st.auth
        .audit(
            Some(&me.user.username),
            "corpus.delete",
            Some(&name),
            None,
            Some(&ip),
        )
        .await;
    st.publish(Event::Corpus {
        event: CorpusEvent::Removed,
        name,
    });
    Ok(StatusCode::NO_CONTENT)
}
