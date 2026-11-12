//! The encode job: re-encode a finished render's master with another preset.
//!
//! The master (`<id>.master.mp4`) is kept beside the result by the renderer, so
//! an export never renders the sentence or song again. An older result without
//! a master is encoded from the result itself.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::json;
use ytp_auth::Quota;
use ytp_core::JobState;

use crate::jobs::{Done, Failure, JobRecord, Outcome};
use crate::AppState;

fn failed(msg: impl Into<String>) -> Outcome {
    Outcome {
        result: Err(Failure::Failed(msg.into())),
        report: None,
        options: None,
        warnings: Vec::new(),
    }
}

fn cancelled() -> Outcome {
    Outcome {
        result: Err(Failure::Cancelled),
        report: None,
        options: None,
        warnings: Vec::new(),
    }
}

/// The finished file an export encodes from: the master when it is still
/// there, otherwise the result (renders made before masters were kept).
pub(crate) fn source_file(st: &AppState, source: &JobRecord) -> Option<PathBuf> {
    let result = st.jobs.result_path_of(source);
    let master = ytp_media::master_path(&result);
    if master.is_file() {
        return Some(master);
    }
    let video = result
        .extension()
        .and_then(|x| x.to_str())
        .is_some_and(|x| matches!(x, "mp4" | "mkv"));
    if video && result.is_file() {
        Some(result)
    } else {
        None
    }
}

/// Run a queued encode: `rec.options` holds `source_job` and `preset`.
pub(crate) async fn run(
    st: &AppState,
    id: &str,
    rec: &JobRecord,
    quota: Quota,
    cancel: Arc<AtomicBool>,
) -> Outcome {
    let source_id = rec.options["source_job"].as_str().unwrap_or_default().to_string();
    let preset_name = rec.options["preset"].as_str().unwrap_or_default().to_string();
    if source_id.is_empty() || preset_name.is_empty() {
        return failed("this export job does not say what to encode");
    }
    let Some(source) = st.jobs.get(&source_id) else {
        return failed("the render this export was made from has gone");
    };
    if source.state != JobState::Done {
        return failed("the render this export was made from is not finished");
    }
    let Some(input) = source_file(st, &source) else {
        return failed("the render this export was made from has no video file to encode");
    };
    let Some(preset) = st.encoder.presets().get(&preset_name).cloned() else {
        return failed(format!("no encode preset named {preset_name:?}"));
    };

    let ext = preset.extension();
    let out = if ext == "mp4" {
        st.jobs.result_path(id)
    } else {
        st.jobs.output_path(&format!("{id}.{ext}"))
    };
    let file = (ext != "mp4").then(|| format!("{id}.{ext}"));

    let st2 = st.clone();
    let id2 = id.to_string();
    let out2 = out.clone();
    let preset2 = preset.clone();
    let message = format!("encoding as {preset_name}");
    let joined = tokio::task::spawn_blocking(move || {
        let s = &st2;
        let progress = |f: f32| {
            s.jobs
                .set_progress(&id2, 0.02 + 0.96 * f.clamp(0.0, 1.0), message.clone());
        };
        let cancel = || cancel.load(Ordering::Relaxed);
        let overrides = ytp_encode::Overrides {
            max_width: 0,
            max_height: quota.max_height,
        };
        s.encoder
            .encode(&input, &preset2, overrides, &out2, &progress, &cancel)
    })
    .await;

    let out_result = match joined {
        Ok(r) => r,
        Err(e) => return failed(format!("the encoder crashed: {e}")),
    };
    match out_result {
        Ok(o) => {
            st.jobs.set_progress(id, 1.0, "done".into());
            Outcome {
                result: Ok(Done {
                    timeline: json!([]),
                    duration: o.duration,
                    size_bytes: o.bytes,
                    file,
                }),
                report: Some(json!({
                    "source_job": source_id,
                    "preset": preset.to_json(),
                    "width": o.width,
                    "height": o.height,
                    "video_kbps": o.video_kbps,
                    "two_pass": o.two_pass,
                    "hardware": o.hardware,
                })),
                options: None,
                warnings: o.warnings,
            }
        }
        Err(ytp_encode::EncodeError::Cancelled) => {
            let _ = std::fs::remove_file(&out);
            cancelled()
        }
        Err(e) => {
            let _ = std::fs::remove_file(&out);
            failed(format!("the encode failed: {e}"))
        }
    }
}
