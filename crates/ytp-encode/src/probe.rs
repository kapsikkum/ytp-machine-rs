//! A small ffprobe wrapper: duration, streams, size and frame rate.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Serialize;
use thiserror::Error;

use crate::run;

#[derive(Debug, Error)]
pub enum ProbeError {
    #[error("cannot run ffprobe: {0}")]
    Unavailable(String),
    #[error("{0}")]
    Failed(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct Stream {
    pub index: u32,
    pub kind: String,
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Probe {
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub has_video: bool,
    pub has_audio: bool,
    pub streams: Vec<Stream>,
}

/// `ffprobe` beside the configured ffmpeg, or the one on PATH.
pub fn sibling_ffprobe(ffmpeg: &Path) -> PathBuf {
    let name = ffmpeg.file_name().map(|n| n.to_string_lossy().to_lowercase());
    if let (Some(dir), Some(name)) = (ffmpeg.parent(), name) {
        if name.starts_with("ffmpeg") {
            let ext = if name.ends_with(".exe") { ".exe" } else { "" };
            let cand = dir.join(format!("ffprobe{ext}"));
            if cand.is_file() {
                return cand;
            }
        }
    }
    PathBuf::from("ffprobe")
}

fn number(v: &serde_json::Value) -> f64 {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|t| t.parse().ok()))
        .unwrap_or(0.0)
}

fn rate(v: &serde_json::Value) -> f64 {
    let Some(text) = v.as_str() else {
        return number(v);
    };
    let (n, d) = text.split_once('/').unwrap_or((text, "1"));
    match (n.parse::<f64>(), d.parse::<f64>()) {
        (Ok(n), Ok(d)) if d != 0.0 => n / d,
        _ => 0.0,
    }
}

impl Probe {
    pub fn parse(json: &[u8]) -> Result<Probe, ProbeError> {
        let v: serde_json::Value =
            serde_json::from_slice(json).map_err(|e| ProbeError::Failed(format!("ffprobe answered with nonsense: {e}")))?;
        let mut streams = Vec::new();
        let mut width = 0u32;
        let mut height = 0u32;
        let mut fps = 0.0f64;
        let mut has_video = false;
        let mut has_audio = false;
        for s in v["streams"].as_array().into_iter().flatten() {
            let kind = s["codec_type"].as_str().unwrap_or_default().to_string();
            let (mut w, mut h) = (
                s["width"].as_u64().unwrap_or(0) as u32,
                s["height"].as_u64().unwrap_or(0) as u32,
            );
            let rotated = s["side_data_list"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|d| number(&d["rotation"]) % 180.0 != 0.0)
                || number(&s["tags"]["rotate"]) % 180.0 != 0.0;
            if rotated && w != 0 && h != 0 {
                std::mem::swap(&mut w, &mut h);
            }
            let r_raw = rate(&s["r_frame_rate"]);
            let avg = rate(&s["avg_frame_rate"]);
            let nb = s["nb_frames"]
                .as_u64()
                .or_else(|| s["nb_frames"].as_str().and_then(|t| t.parse().ok()))
                .unwrap_or(0);
            let sdur = number(&s["duration"]);
            // For some containers (GIF) avg_frame_rate is derived from the
            // container's centisecond delays and lies; the frame count does not.
            let mut r = if avg > 0.0 { avg } else { r_raw };
            if nb > 0 && sdur > 0.0 {
                let computed = nb as f64 / sdur;
                if r_raw > 0.0 && (computed - r_raw).abs() < 0.5 {
                    r = r_raw;
                }
            }
            match kind.as_str() {
                "video" => {
                    has_video = true;
                    if width == 0 && w != 0 {
                        width = w;
                        height = h;
                        fps = r;
                    }
                }
                "audio" => has_audio = true,
                _ => {}
            }
            streams.push(Stream {
                index: s["index"].as_u64().unwrap_or(0) as u32,
                kind,
                codec: s["codec_name"].as_str().unwrap_or_default().to_string(),
                width: w,
                height: h,
                fps: r,
            });
        }
        let mut duration = number(&v["format"]["duration"]);
        if duration <= 0.0 {
            duration = v["streams"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|s| number(&s["duration"]))
                .fold(0.0f64, f64::max);
        }
        Ok(Probe {
            duration,
            width,
            height,
            fps,
            has_video,
            has_audio,
            streams,
        })
    }
}

/// Run ffprobe over `input`.
pub fn probe(ffprobe: &Path, input: &Path) -> Result<Probe, ProbeError> {
    let mut cmd = Command::new(ffprobe);
    cmd.args([
        "-v",
        "error",
        "-show_entries",
        "format=duration:stream=index,codec_type,codec_name,width,height,r_frame_rate,avg_frame_rate,duration,nb_frames:stream_side_data=rotation:stream_tags=rotate",
        "-of",
        "json",
    ])
    .arg(input);
    let out = run::capture(&mut cmd, Duration::from_secs(30)).map_err(|e| match e {
        run::RunError::Spawn(e) => ProbeError::Unavailable(format!("{}: {e}", ffprobe.display())),
        run::RunError::Cancelled | run::RunError::Timeout => {
            ProbeError::Failed("ffprobe did not answer".into())
        }
    })?;
    if !out.success {
        return Err(ProbeError::Failed(format!(
            "ffprobe could not read {}: {}",
            input.display(),
            first_line(&out.stderr)
        )));
    }
    Probe::parse(&out.stdout)
}

fn first_line(text: &str) -> String {
    text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string()
}
