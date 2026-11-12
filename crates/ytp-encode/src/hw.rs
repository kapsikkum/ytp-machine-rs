//! Which hardware encoders this machine's ffmpeg can actually use.
//!
//! The encoder list alone lies (a build can include NVENC with no driver), so
//! each candidate is tried on two black frames. The answer is cached per
//! ffmpeg binary for the life of the process.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;

use crate::run;

/// Probe order also decides preference: NVENC, then QSV, then AMF.
pub const CANDIDATES: &[&str] = &["h264_nvenc", "hevc_nvenc", "h264_qsv", "h264_amf"];

#[derive(Debug, Clone, Serialize)]
pub struct HwEncoder {
    pub encoder: String,
    pub available: bool,
    /// Why it is not usable, or that the test encode worked.
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HwReport {
    /// The ffmpeg binary that was asked.
    pub ffmpeg: String,
    /// Whether ffmpeg could be run at all.
    pub ok: bool,
    pub encoders: Vec<HwEncoder>,
}

impl HwReport {
    pub fn available(&self, encoder: &str) -> bool {
        self.encoders
            .iter()
            .any(|e| e.encoder == encoder && e.available)
    }

    /// NVENC > QSV > AMF among the encoders that work.
    pub fn preferred_h264(&self) -> Option<&'static str> {
        ["h264_nvenc", "h264_qsv", "h264_amf"]
            .into_iter()
            .find(|e| self.available(e))
    }

    pub fn preferred_hevc(&self) -> Option<&'static str> {
        ["hevc_nvenc"].into_iter().find(|e| self.available(e))
    }
}

fn cache() -> &'static Mutex<HashMap<String, Arc<HwReport>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<HwReport>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The cached report for `ffmpeg`, probing once.
pub fn report(ffmpeg: &Path) -> Arc<HwReport> {
    let key = ffmpeg.display().to_string();
    if let Some(r) = cache().lock().unwrap().get(&key) {
        return r.clone();
    }
    let probed = Arc::new(probe(ffmpeg));
    cache().lock().unwrap().insert(key, probed.clone());
    probed
}

/// Forget a cached report (tests, or an admin asking again).
pub fn forget(ffmpeg: &Path) {
    cache().lock().unwrap().remove(&ffmpeg.display().to_string());
}

fn probe(ffmpeg: &Path) -> HwReport {
    let listed = match encoder_list(ffmpeg) {
        Ok(list) => list,
        Err(e) => {
            return HwReport {
                ffmpeg: ffmpeg.display().to_string(),
                ok: false,
                encoders: CANDIDATES
                    .iter()
                    .map(|name| HwEncoder {
                        encoder: (*name).to_string(),
                        available: false,
                        detail: e.clone(),
                    })
                    .collect(),
            };
        }
    };
    let encoders = CANDIDATES
        .iter()
        .map(|name| {
            if !listed.contains(*name) {
                return HwEncoder {
                    encoder: (*name).to_string(),
                    available: false,
                    detail: "this ffmpeg does not include it".into(),
                };
            }
            match test_encode(ffmpeg, name) {
                Ok(()) => HwEncoder {
                    encoder: (*name).to_string(),
                    available: true,
                    detail: "test encode worked".into(),
                },
                Err(e) => HwEncoder {
                    encoder: (*name).to_string(),
                    available: false,
                    detail: e,
                },
            }
        })
        .collect();
    HwReport {
        ffmpeg: ffmpeg.display().to_string(),
        ok: true,
        encoders,
    }
}

fn encoder_list(ffmpeg: &Path) -> Result<String, String> {
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-encoders"]);
    match run::capture(&mut cmd, Duration::from_secs(20)) {
        Ok(o) if o.success => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
        Ok(o) => Err(format!(
            "{} -encoders exited with {:?}",
            ffmpeg.display(),
            o.code
        )),
        Err(run::RunError::Spawn(e)) => Err(format!("cannot run {}: {e}", ffmpeg.display())),
        Err(_) => Err(format!("{} did not answer", ffmpeg.display())),
    }
}

fn test_encode(ffmpeg: &Path, encoder: &str) -> Result<(), String> {
    let mut cmd = Command::new(ffmpeg);
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-f",
        "lavfi",
        "-i",
        "color=c=black:s=256x144:r=25:d=0.2",
        "-frames:v",
        "2",
        "-an",
        "-c:v",
        encoder,
        "-pix_fmt",
        "yuv420p",
        "-f",
        "null",
        "-",
    ]);
    match run::capture(&mut cmd, Duration::from_secs(25)) {
        Ok(o) if o.success => Ok(()),
        Ok(o) => {
            let detail = o
                .stderr
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("the test encode failed")
                .trim()
                .chars()
                .take(240)
                .collect();
            Err(detail)
        }
        Err(run::RunError::Spawn(e)) => Err(format!("cannot run {}: {e}", ffmpeg.display())),
        Err(_) => Err("the test encode timed out".into()),
    }
}
