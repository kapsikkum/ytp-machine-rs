//! What this machine's ffmpeg can do, asked once: filters, encoders, a
//! caption font, and whether a source is already in the output format.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use ytp_splice::Segment;

use crate::graph::{GraphEnv, FPS};
use crate::proc::{self, RunError};

const FONTS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "C:/Windows/Fonts/arialbd.ttf",
    "C:/Windows/Fonts/arial.ttf",
    "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoder {
    X264,
    Nvenc,
    Qsv,
}

impl Encoder {
    pub fn from_env() -> Self {
        match std::env::var("YTP_ENCODER").unwrap_or_default().to_lowercase().trim() {
            "nvenc" => Encoder::Nvenc,
            "qsv" => Encoder::Qsv,
            _ => Encoder::X264,
        }
    }
}

pub struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    encoder: Encoder,
    filters: OnceLock<HashSet<String>>,
    encoders: OnceLock<String>,
    font: OnceLock<Option<String>>,
    #[allow(clippy::type_complexity)]
    native: Mutex<HashMap<(PathBuf, Option<SystemTime>, u64), bool>>,
}

impl Tools {
    /// `ffmpeg` is the binary to run; ffprobe is looked for beside it.
    pub fn new(ffmpeg: impl Into<PathBuf>) -> Self {
        let ffmpeg = ffmpeg.into();
        let ffprobe = sibling_probe(&ffmpeg);
        Self {
            ffmpeg,
            ffprobe,
            encoder: Encoder::from_env(),
            filters: OnceLock::new(),
            encoders: OnceLock::new(),
            font: OnceLock::new(),
            native: Mutex::new(HashMap::new()),
        }
    }

    fn capture(&self, args: &[&str]) -> Option<String> {
        let mut cmd = std::process::Command::new(&self.ffmpeg);
        cmd.args(args);
        let out = proc::run(&mut cmd, Duration::from_secs(15), &|| true).ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// `ffmpeg -version`'s first line, or the reason it cannot run.
    pub fn version(&self) -> Result<String, String> {
        let mut cmd = std::process::Command::new(&self.ffmpeg);
        cmd.arg("-version");
        match proc::run(&mut cmd, Duration::from_secs(15), &|| true) {
            Ok(o) if o.success => Ok(String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or_default()
                .to_string()),
            Ok(o) => Err(format!("{} exited with {:?}", self.ffmpeg.display(), o.code)),
            Err(RunError::Spawn(e)) => Err(format!("cannot run {}: {e}", self.ffmpeg.display())),
            Err(_) => Err(format!("{} did not answer", self.ffmpeg.display())),
        }
    }

    pub fn filters(&self) -> &HashSet<String> {
        self.filters.get_or_init(|| {
            self.capture(&["-hide_banner", "-filters"])
                .map(|o| {
                    o.lines()
                        .filter_map(|l| l.split_whitespace().nth(1).map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        })
    }

    /// The H.264 codec flags for the configured encoder, falling back to
    /// libx264 ultrafast when the hardware one is missing.
    pub fn encoder_args(&self) -> Vec<String> {
        let list = self
            .encoders
            .get_or_init(|| self.capture(&["-hide_banner", "-encoders"]).unwrap_or_default());
        let args: &[&str] = match self.encoder {
            Encoder::Nvenc if list.contains("h264_nvenc") => &[
                "-c:v", "h264_nvenc", "-preset", "p1", "-tune", "ull", "-rc", "vbr", "-cq", "23",
            ],
            Encoder::Qsv if list.contains("h264_qsv") => {
                &["-c:v", "h264_qsv", "-preset", "veryfast", "-global_quality", "23"]
            }
            _ => &["-c:v", "libx264", "-preset", "ultrafast", "-crf", "23"],
        };
        args.iter().map(|s| s.to_string()).collect()
    }

    pub fn caption_font(&self) -> Option<String> {
        self.font
            .get_or_init(|| {
                if let Ok(p) = std::env::var("YTP_FONT") {
                    if Path::new(&p).is_file() {
                        return Some(p);
                    }
                }
                let found = FONTS.iter().find(|p| Path::new(p).is_file()).map(|p| p.to_string());
                if found.is_none() {
                    tracing::warn!("no font found for captions; they will be skipped");
                }
                found
            })
            .clone()
    }

    /// Is the file 480x270 at 25 fps with no rotation? Probed once per version of a file.
    pub fn native_format(&self, path: &Path) -> bool {
        let Ok(meta) = std::fs::metadata(path) else { return false };
        let key = (path.to_path_buf(), meta.modified().ok(), meta.len());
        if let Some(v) = self.native.lock().unwrap().get(&key) {
            return *v;
        }
        let ok = self.probe_native(path);
        self.native.lock().unwrap().insert(key, ok);
        ok
    }

    fn probe_native(&self, path: &Path) -> bool {
        let mut cmd = std::process::Command::new(&self.ffprobe);
        cmd.args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,r_frame_rate,avg_frame_rate:stream_side_data=rotation:stream_tags=rotate",
            "-of",
            "json",
        ])
        .arg(path);
        let Ok(out) = proc::run(&mut cmd, Duration::from_secs(10), &|| true) else {
            return false;
        };
        if !out.success {
            return false;
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
            return false;
        };
        let Some(s) = v["streams"].get(0) else { return false };
        let num = |x: &serde_json::Value| -> f64 {
            x.as_f64()
                .or_else(|| x.as_str().and_then(|t| t.parse().ok()))
                .unwrap_or(0.0)
        };
        let rotated = std::iter::once(&s["tags"]["rotate"])
            .chain(
                s["side_data_list"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|d| &d["rotation"]),
            )
            .any(|r| num(r) % 360.0 != 0.0);
        let rate = |x: &serde_json::Value| -> Option<f64> {
            let t = x.as_str()?;
            let (n, d) = t.split_once('/').unwrap_or((t, "1"));
            let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
            (d != 0.0).then_some(n / d)
        };
        !rotated
            && s["width"].as_i64() == Some(480)
            && s["height"].as_i64() == Some(270)
            && rate(&s["r_frame_rate"]) == Some(FPS as f64)
            && rate(&s["avg_frame_rate"]) == Some(FPS as f64)
    }
}

fn sibling_probe(ffmpeg: &Path) -> PathBuf {
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

impl GraphEnv for Tools {
    fn has_filter(&self, name: &str) -> bool {
        self.filters().contains(name)
    }

    fn is_native(&self, seg: &Segment) -> bool {
        !seg.source_file.is_empty() && self.native_format(Path::new(&seg.source_file))
    }

    fn font(&self) -> Option<String> {
        self.caption_font()
    }
}
