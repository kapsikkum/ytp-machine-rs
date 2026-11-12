//! Decoding source audio and video through the ffmpeg CLI
//! (`pitch.decode_audio` and `Sample._frames_ffmpeg`).

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use ytp_media::proc::{run, trim_stderr, RunError};

use crate::{Error, Result, FPS, SR};

fn run_err(e: RunError, ffmpeg: &Path) -> Error {
    match e {
        RunError::Cancelled => Error::Cancelled,
        RunError::Timeout => Error::Msg("ffmpeg took too long".into()),
        RunError::Spawn(e) => Error::Internal(format!("cannot run {}: {e}", ffmpeg.display())),
    }
}

/// Mono float32 audio at [`SR`] from anything ffmpeg can read.
pub fn decode_audio(
    ffmpeg: &Path,
    path: &str,
    start: Option<f64>,
    duration: Option<f64>,
    keep_going: &dyn Fn() -> bool,
) -> Result<Vec<f32>> {
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-v", "error", "-nostdin", "-threads", "2", "-filter_threads", "2"]);
    if let Some(s) = start {
        cmd.args(["-ss", &format!("{:.4}", s.max(0.0))]);
    }
    if let Some(d) = duration {
        cmd.args(["-t", &format!("{d:.4}")]);
    }
    cmd.args(["-i", path, "-vn", "-ac", "1", "-ar", &SR.to_string(), "-threads", "2", "-f", "f32le", "-"]);
    let out = run(&mut cmd, Duration::from_secs(60), keep_going).map_err(|e| run_err(e, ffmpeg))?;
    if !out.success {
        return Err(Error::Msg(format!("FFmpeg failed: {}", trim_stderr(&out.stderr, 400))));
    }
    let raw = &out.stdout[..out.stdout.len() / 4 * 4];
    Ok(raw.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect())
}

/// Decoded RGB24 frames, one every 1/[`FPS`] s.
pub struct Frames {
    pub n: usize,
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

impl Frames {
    pub fn frame(&self, i: usize) -> &[u8] {
        let size = self.w * self.h * 3;
        let i = i.min(self.n.saturating_sub(1));
        &self.data[i * size..(i + 1) * size]
    }
}

/// The picture at `w`x`h`, from `offset` for `duration` seconds.
///
/// Retro canvases can have non-square display pixels, so the picture is fitted
/// against the final picture's aspect (`pixel_aspect`) and the later canvas
/// upscale does not stretch it again.
pub fn decode_video(
    ffmpeg: &Path,
    path: &Path,
    offset: f64,
    duration: f64,
    w: usize,
    h: usize,
    cover: bool,
    pixel_aspect: f64,
    keep_going: &dyn Fn() -> bool,
) -> Result<Frames> {
    let bound = if cover { "max" } else { "min" };
    let pa = pixel_aspect;
    let mut framing = format!("scale=w='max(1,{bound}({w},{h}*dar/{pa}))':h='max(1,{bound}({h},{w}*{pa}/dar))'");
    if cover {
        framing += &format!(",crop={w}:{h}:exact=1");
    } else {
        framing += &format!(",pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=black");
    }
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-v", "error", "-nostdin", "-threads", "2", "-filter_threads", "2"]);
    cmd.args(["-ss", &format!("{offset:.3}")]);
    cmd.arg("-i").arg(path);
    cmd.args(["-t", &format!("{:.3}", duration + 0.1)]);
    cmd.args(["-vf", &format!("format=rgb24,{framing},setsar=1,fps={FPS}")]);
    cmd.args(["-threads", "2", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"]);
    let out = run(&mut cmd, Duration::from_secs(120), keep_going).map_err(|e| run_err(e, ffmpeg))?;
    if !out.success {
        return Err(Error::Msg(format!("FFmpeg failed: {}", trim_stderr(&out.stderr, 400))));
    }
    let size = w * h * 3;
    let n = out.stdout.len() / size;
    if n == 0 {
        return Ok(Frames { n: 1, w, h, data: vec![0; size] });
    }
    let mut data = out.stdout;
    data.truncate(n * size);
    Ok(Frames { n, w, h, data })
}
