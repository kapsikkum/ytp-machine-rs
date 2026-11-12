//! Shared encode core: everything in this workspace that writes a video file
//! goes through here.
//!
//! * [`PresetSet`] — the data-defined presets (built-in TOML plus
//!   `<data>/presets/*.toml`).
//! * [`Encoder::encode`] — input → one output file, H.264/H.265/AAC/Opus/GIF,
//!   constant quality, fixed bitrate or a two-pass target size, with hardware
//!   encoders when the preset allows and the machine has them.
//! * [`Probe`] — ffprobe, and [`ProgressParser`] — `-progress pipe:1` into a
//!   fraction, with cancellation that kills the child.

#![allow(clippy::too_many_arguments)]

pub mod hw;
pub mod preset;
pub mod probe;
pub mod progress;
pub(crate) mod run;
pub mod target;

use std::path::{Path, PathBuf};
use std::time::Duration;

use thiserror::Error;

pub use crate::hw::{HwEncoder, HwReport};
pub use crate::preset::{
    AudioCodec, Container, HwMode, Preset, PresetError, PresetSet, Quality, VideoCodec,
};
pub use crate::probe::{Probe, ProbeError, Stream};
pub use crate::progress::ProgressParser;
pub use crate::target::TargetPlan;

/// Called with a fraction of the finished encode (`0..=1`).
pub type ProgressFn<'a> = &'a dyn Fn(f32);
/// Return `true` to stop the encode (the child process is killed).
pub type CancelFn<'a> = &'a dyn Fn() -> bool;

/// Per-call limits layered on top of the preset, e.g. the caller's role quota.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Overrides {
    /// 0 = no limit beyond the preset's own.
    pub max_width: u32,
    /// 0 = no limit beyond the preset's own.
    pub max_height: u32,
}

impl Overrides {
    pub fn max_height(height: u32) -> Self {
        Self {
            max_width: 0,
            max_height: height,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EncodeOutput {
    pub path: PathBuf,
    pub bytes: u64,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    /// The video bitrate asked for (target-size and bitrate modes).
    pub video_kbps: Option<u32>,
    pub two_pass: bool,
    pub hardware: Option<String>,
    /// Things that went wrong but did not stop the encode (a target that was
    /// still a little too big after the retry).
    pub warnings: Vec<String>,
}

#[derive(Debug, Error)]
pub enum EncodeError {
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Failed(String),
    #[error("ffmpeg is not available: {0}")]
    Unavailable(String),
}

pub struct Encoder {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
    presets: PresetSet,
}

impl Encoder {
    /// An encoder using the built-in presets.
    pub fn new(ffmpeg: impl Into<PathBuf>) -> Self {
        Self::with_presets(ffmpeg, PresetSet::builtin().clone())
    }

    pub fn with_presets(ffmpeg: impl Into<PathBuf>, presets: PresetSet) -> Self {
        let ffmpeg = ffmpeg.into();
        let ffprobe = probe::sibling_ffprobe(&ffmpeg);
        Self {
            ffmpeg,
            ffprobe,
            presets,
        }
    }

    pub fn ffmpeg(&self) -> &Path {
        &self.ffmpeg
    }

    pub fn ffprobe(&self) -> &Path {
        &self.ffprobe
    }

    pub fn presets(&self) -> &PresetSet {
        &self.presets
    }

    /// Cached hardware encoder probe for this ffmpeg.
    pub fn hardware(&self) -> std::sync::Arc<HwReport> {
        hw::report(&self.ffmpeg)
    }

    pub fn probe(&self, input: &Path) -> Result<Probe, EncodeError> {
        probe::probe(&self.ffprobe, input).map_err(map_probe)
    }

    /// Encode `input` to `out` with `preset`, honouring `overrides`.
    pub fn encode(
        &self,
        input: &Path,
        preset: &Preset,
        overrides: Overrides,
        out: &Path,
        progress: ProgressFn,
        cancel: CancelFn,
    ) -> Result<EncodeOutput, EncodeError> {
        let input_probe = self.probe(input)?;
        if input_probe.duration <= 0.0 {
            return Err(EncodeError::Failed(format!(
                "{} has no readable duration",
                input.display()
            )));
        }
        if preset.is_audio_only() {
            return self.encode_audio(input, preset, &input_probe, out, progress, cancel);
        }
        if !input_probe.has_video {
            return Err(EncodeError::Failed(format!(
                "{} has no video stream to encode",
                input.display()
            )));
        }
        if preset.is_gif() {
            return self.encode_gif(input, preset, overrides, &input_probe, out, progress, cancel);
        }
        self.encode_video(input, preset, overrides, &input_probe, out, progress, cancel)
    }

    fn max_dims(&self, preset: &Preset, overrides: Overrides, probe: &Probe) -> (u32, u32) {
        let max_w = min_limit(preset.max_width, overrides.max_width);
        let max_h = min_limit(preset.max_height, overrides.max_height);
        target::fit_even(probe.width, probe.height, max_w, max_h)
    }

    fn encode_audio(
        &self,
        input: &Path,
        preset: &Preset,
        probe: &Probe,
        out: &Path,
        progress: ProgressFn,
        cancel: CancelFn,
    ) -> Result<EncodeOutput, EncodeError> {
        if !probe.has_audio {
            return Err(EncodeError::Failed(format!(
                "{} has no audio stream to encode",
                input.display()
            )));
        }
        let kbps = preset.bitrate_kbps.unwrap_or(preset.audio_kbps);
        let mut args = base_args(input, out);
        args.extend(["-map", "0:a:0", "-vn"].map(String::from));
        args.extend(audio_args(preset.audio_codec, kbps));
        if preset.container == Container::M4a && preset.faststart {
            args.extend(["-movflags", "+faststart"].map(String::from));
        }
        args.extend(["-f", muxer(preset.container)].map(String::from));
        args.extend(preset.extra_args.iter().cloned());
        args.push(out.display().to_string());
        self.run_encoded(&args, probe.duration, 0.0, 1.0, progress, cancel)?;
        self.collect(out, probe.duration, 0, 0, None, false, None, Vec::new(), cancel, progress)
    }

    fn encode_gif(
        &self,
        input: &Path,
        preset: &Preset,
        overrides: Overrides,
        probe: &Probe,
        out: &Path,
        progress: ProgressFn,
        cancel: CancelFn,
    ) -> Result<EncodeOutput, EncodeError> {
        let (w, h) = self.max_dims(preset, overrides, probe);
        let fps = target::cap_fps(probe.fps, preset.fps_cap);
        let vf = format!(
            "fps={},scale={w}:{h}:flags=lanczos:force_original_aspect_ratio=decrease:force_divisible_by=2,split[s0][s1];[s0]palettegen=stats_mode=diff[p];[s1][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle",
            num(fps)
        );
        let mut args = base_args(input, out);
        args.extend(["-filter_complex", &vf, "-an"].map(String::from));
        args.extend(preset.extra_args.iter().cloned());
        args.extend(["-f", "gif"].map(String::from));
        args.push(out.display().to_string());
        self.run_encoded(&args, probe.duration, 0.0, 1.0, progress, cancel)?;
        self.collect(out, probe.duration, w, h, None, false, None, Vec::new(), cancel, progress)
    }

    #[allow(clippy::too_many_arguments)]
    fn encode_video(
        &self,
        input: &Path,
        preset: &Preset,
        overrides: Overrides,
        probe: &Probe,
        out: &Path,
        progress: ProgressFn,
        cancel: CancelFn,
    ) -> Result<EncodeOutput, EncodeError> {
        let (fit_w, fit_h) = self.max_dims(preset, overrides, probe);
        let two_pass = preset.quality == Quality::TargetSizeMb;
        let mut warnings = Vec::new();

        // Target size: the bitrate decides the resolution; CRF/bitrate: the
        // preset's limits decide both.
        let mut plan = match preset.quality {
            Quality::TargetSizeMb => {
                let max_w = min_limit(preset.max_width, overrides.max_width);
                let max_h = min_limit(preset.max_height, overrides.max_height);
                let target = preset
                    .target_bytes()
                    .ok_or_else(|| EncodeError::Failed(format!("{} has no target size", preset.name)))?;
                let audio = if preset.audio_codec == AudioCodec::None || !probe.has_audio {
                    0
                } else {
                    preset.audio_kbps
                };
                target::plan_target_size(
                    probe.duration,
                    target,
                    audio,
                    probe.width,
                    probe.height,
                    probe.fps,
                    max_w,
                    max_h,
                    preset.fps_cap,
                )
                .map_err(EncodeError::Failed)?
            }
            Quality::Crf | Quality::Bitrate => {
                let fps = target::cap_fps(probe.fps, preset.fps_cap);
                let kbps = preset.bitrate_kbps.unwrap_or(0);
                TargetPlan {
                    width: fit_w,
                    height: fit_h,
                    fps,
                    video_kbps: kbps,
                    target_bytes: 0,
                    scaled: false,
                }
            }
        };
        let hardware = self.choose_hardware(preset, two_pass);
        let pass_dir = tempfile::Builder::new()
            .prefix("ytp-encode-pass-")
            .tempdir()
            .map_err(|e| EncodeError::Failed(format!("cannot make a temp dir: {e}")))?;
        let passlog = pass_dir.path().join("ffmpeg2pass");
        let mut attempt = 0usize;
        loop {
            if attempt > 0 {
                progress(0.0);
            }
            if two_pass {
                let mut pass1 = self.video_args(
                    preset,
                    input,
                    probe,
                    &plan,
                    Some(1),
                    Some(&passlog),
                    None,
                );
                pass1.extend(["-an", "-f", "null", "-"].map(String::from));
                self.run_encoded(&pass1, probe.duration, 0.0, 0.45, progress, cancel)?;
                let mut pass2 = self.video_args(
                    preset,
                    input,
                    probe,
                    &plan,
                    Some(2),
                    Some(&passlog),
                    None,
                );
                self.with_output(&mut pass2, preset, out, probe);
                self.run_encoded(&pass2, probe.duration, 0.45, 0.99, progress, cancel)?;
            } else {
                let mut args = self.video_args(
                    preset,
                    input,
                    probe,
                    &plan,
                    None,
                    None,
                    hardware.as_deref(),
                );
                self.with_output(&mut args, preset, out, probe);
                self.run_encoded(&args, probe.duration, 0.0, 0.99, progress, cancel)?;
            }
            let out_bytes = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
            if out_bytes == 0 {
                return Err(EncodeError::Failed("the encoder produced no file".into()));
            }
            // Under the asked size, or one try with a lower bitrate is used up.
            let Some(target) = preset.target_bytes() else { break };
            if out_bytes <= target || attempt > 0 {
                if out_bytes > target {
                    warnings.push(format!(
                        "still {:.1} MB after trying a lower bitrate (the target is {:.1} MB)",
                        out_bytes as f64 / 1e6,
                        target as f64 / 1e6
                    ));
                }
                break;
            }
            let next = (plan.video_kbps as f64 * target as f64 / out_bytes as f64 * 0.97)
                .floor()
                .max(target::MIN_VIDEO_KBPS as f64) as u32;
            if next >= plan.video_kbps {
                warnings.push(format!(
                    "the output is {:.1} MB, above the {:.1} MB target",
                    out_bytes as f64 / 1e6,
                    target as f64 / 1e6
                ));
                break;
            }
            tracing::info!(
                preset = %preset.name,
                from_kbps = plan.video_kbps,
                to_kbps = next,
                bytes = out_bytes,
                "target size missed; retrying once with a lower bitrate"
            );
            plan.video_kbps = next;
            attempt += 1;
        }

        let kbps = (two_pass || preset.quality == Quality::Bitrate).then_some(plan.video_kbps);
        self.collect(
            out,
            probe.duration,
            plan.width,
            plan.height,
            kbps,
            two_pass,
            hardware.clone(),
            warnings,
            cancel,
            progress,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn collect(
        &self,
        out: &Path,
        duration: f64,
        width: u32,
        height: u32,
        video_kbps: Option<u32>,
        two_pass: bool,
        hardware: Option<String>,
        warnings: Vec<String>,
        cancel: CancelFn,
        progress: ProgressFn,
    ) -> Result<EncodeOutput, EncodeError> {
        let bytes = std::fs::metadata(out)
            .map_err(|e| EncodeError::Failed(format!("{} was not written: {e}", out.display())))?
            .len();
        if bytes == 0 {
            return Err(EncodeError::Failed("the encoder produced an empty file".into()));
        }
        if cancel() {
            let _ = std::fs::remove_file(out);
            return Err(EncodeError::Cancelled);
        }
        let probed = self.probe(out).ok();
        let duration = probed
            .as_ref()
            .map(|p| p.duration)
            .filter(|d| *d > 0.0)
            .unwrap_or(duration);
        let width = probed
            .as_ref()
            .map(|p| p.width)
            .filter(|w| *w > 0)
            .unwrap_or(width);
        let height = probed
            .as_ref()
            .map(|p| p.height)
            .filter(|h| *h > 0)
            .unwrap_or(height);
        progress(1.0);
        Ok(EncodeOutput {
            path: out.to_path_buf(),
            bytes,
            duration,
            width,
            height,
            video_kbps,
            two_pass,
            hardware,
            warnings,
        })
    }

    fn choose_hardware(&self, preset: &Preset, two_pass: bool) -> Option<String> {
        // Two-pass is libx264 only, and NVENC here rejects gop = 1 outright.
        if two_pass || preset.hw != HwMode::Auto || preset.gop == 1 {
            return None;
        }
        let report = self.hardware();
        match preset.video_codec {
            VideoCodec::H264 => report.preferred_h264(),
            VideoCodec::H265 => report.preferred_hevc(),
            _ => None,
        }
        .map(str::to_string)
    }

    /// Video args shared by single-pass and both two-pass passes.
    fn video_args(
        &self,
        preset: &Preset,
        input: &Path,
        probe: &Probe,
        plan: &TargetPlan,
        pass: Option<u8>,
        passlog: Option<&Path>,
        hardware: Option<&str>,
    ) -> Vec<String> {
        let mut args = ["-y", "-hide_banner", "-loglevel", "error", "-nostdin"]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        args.push("-progress".into());
        args.push("pipe:1".into());
        args.push("-nostats".into());
        args.push("-i".into());
        args.push(input.display().to_string());
        args.extend(["-map", "0:v:0"].map(String::from));
        if preset.audio_codec != AudioCodec::None && probe.has_audio {
            args.extend(["-map", "0:a:0?"].map(String::from));
        }
        let mut vf: Vec<String> = Vec::new();
        if plan.width != probe.width || plan.height != probe.height {
            vf.push(format!("scale={}:{}:flags=lanczos", plan.width, plan.height));
        }
        if (plan.fps - probe.fps).abs() > 0.01 {
            vf.push(format!("fps={}", num(plan.fps)));
        }
        if !vf.is_empty() {
            args.extend(["-vf", &vf.join(",")].map(String::from));
        }
        match hardware {
            Some("h264_nvenc") | Some("hevc_nvenc") => {
                args.extend(["-c:v", hardware.unwrap()].map(String::from));
                args.extend(["-preset", "p4", "-tune", "hq", "-rc", "vbr"].map(String::from));
                if let Some(crf) = preset.crf {
                    args.extend(["-cq".into(), num(crf)]);
                }
                args.extend(["-b:v", "0"].map(String::from));
            }
            Some("h264_qsv") => {
                args.extend(["-c:v", "h264_qsv", "-preset", "veryfast"].map(String::from));
                if let Some(crf) = preset.crf {
                    args.extend(["-global_quality".into(), num(crf)]);
                }
            }
            Some("h264_amf") => {
                args.extend(["-c:v", "h264_amf", "-rc", "cqp"].map(String::from));
                if let Some(crf) = preset.crf {
                    let q = num(crf);
                    args.extend(["-qp_i".into(), q.clone(), "-qp_p".into(), q.clone(), "-qp_b".into(), q]);
                }
            }
            _ => {
                let codec = match preset.video_codec {
                    VideoCodec::H265 => "libx265",
                    _ => "libx264",
                };
                args.extend(["-c:v", codec, "-preset", preset.speed.as_str()].map(String::from));
                match preset.quality {
                    Quality::Crf => {
                        if let Some(crf) = preset.crf {
                            args.extend(["-crf".into(), num(crf)]);
                        }
                    }
                    Quality::Bitrate | Quality::TargetSizeMb => {
                        let k = plan.video_kbps.to_string();
                        let maxrate = plan.video_kbps + plan.video_kbps / 10;
                        args.extend([
                            "-b:v".into(),
                            format!("{k}k"),
                            "-maxrate".into(),
                            format!("{maxrate}k"),
                            "-bufsize".into(),
                            format!("{}k", plan.video_kbps * 2),
                        ]);
                    }
                }
                if let (Some(n), Some(log)) = (pass, passlog) {
                    args.extend([
                        "-pass".into(),
                        n.to_string(),
                        "-passlogfile".into(),
                        log.display().to_string(),
                    ]);
                }
            }
        }
        if let Some(tune) = &preset.tune {
            if hardware.is_none() {
                args.extend(["-tune", tune.as_str()].map(String::from));
            }
        }
        if preset.gop > 0 {
            args.extend(["-g".to_string(), preset.gop.to_string()]);
            if preset.gop == 1 {
                // All-intra: no B-frames may be referenced (NVENC rejects
                // gop = 1 otherwise).
                args.extend(["-bf", "0"].map(String::from));
            }
        }
        args.extend(["-pix_fmt", "yuv420p"].map(String::from));
        if preset.audio_codec != AudioCodec::None && probe.has_audio {
            args.extend(audio_args(preset.audio_codec, preset.audio_kbps));
            args.push("-shortest".into());
        }
        args
    }

    fn with_output(&self, args: &mut Vec<String>, preset: &Preset, out: &Path, probe: &Probe) {
        if preset.audio_codec == AudioCodec::None || !probe.has_audio {
            args.push("-an".into());
        }
        if preset.faststart && matches!(preset.container, Container::Mp4 | Container::M4a) {
            args.extend(["-movflags", "+faststart"].map(String::from));
        }
        args.extend(preset.extra_args.iter().cloned());
        args.extend(["-f", muxer(preset.container)].map(String::from));
        args.push(out.display().to_string());
    }

    fn run_encoded(
        &self,
        args: &[String],
        duration: f64,
        from: f32,
        to: f32,
        progress: ProgressFn,
        cancel: CancelFn,
    ) -> Result<(), EncodeError> {
        let span = (to - from).max(0.0);
        let map = |f: f32| progress(from + span * f.clamp(0.0, 1.0));
        let timeout = Duration::from_secs((600.0 + duration.max(1.0) * 20.0) as u64);
        let out = run::run_ticked(&self.ffmpeg, args, duration, &map, cancel, timeout).map_err(
            |e| match e {
                run::RunError::Spawn(e) => {
                    EncodeError::Unavailable(format!("cannot run {}: {e}", self.ffmpeg.display()))
                }
                run::RunError::Cancelled => EncodeError::Cancelled,
                run::RunError::Timeout => EncodeError::Failed("ffmpeg took too long".into()),
            },
        )?;
        if !out.success {
            Err(EncodeError::Failed(format!(
                "ffmpeg failed (exit {:?}):\n{}",
                out.code,
                run::trim_stderr(&out.stderr, 2000)
            )))
        } else {
            Ok(())
        }
    }
}

fn map_probe(e: ProbeError) -> EncodeError {
    match e {
        ProbeError::Unavailable(m) => EncodeError::Unavailable(m),
        ProbeError::Failed(m) => EncodeError::Failed(m),
    }
}

fn min_limit(a: u32, b: u32) -> u32 {
    match (a, b) {
        (0, 0) => 0,
        (0, b) => b,
        (a, 0) => a,
        (a, b) => a.min(b),
    }
}

fn num(v: f64) -> String {
    if (v - v.round()).abs() < 1e-6 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.3}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

fn audio_args(codec: AudioCodec, kbps: u32) -> Vec<String> {
    match codec {
        AudioCodec::Aac => vec![
            "-c:a".into(),
            "aac".into(),
            "-b:a".into(),
            format!("{kbps}k"),
            "-ac".into(),
            "2".into(),
        ],
        AudioCodec::Opus => vec![
            "-c:a".into(),
            "libopus".into(),
            "-b:a".into(),
            format!("{kbps}k"),
            "-ac".into(),
            "2".into(),
        ],
        AudioCodec::None => vec!["-an".into()],
    }
}

fn muxer(container: Container) -> &'static str {
    match container {
        Container::Mp4 => "mp4",
        Container::Mkv => "matroska",
        Container::Gif => "gif",
        Container::M4a => "ipod",
        Container::Ogg => "ogg",
    }
}

fn base_args(input: &Path, _out: &Path) -> Vec<String> {
    let mut args = ["-y", "-hide_banner", "-loglevel", "error", "-nostdin"]
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    args.extend(["-progress", "pipe:1", "-nostats", "-i"].map(String::from));
    args.push(input.display().to_string());
    args
}
