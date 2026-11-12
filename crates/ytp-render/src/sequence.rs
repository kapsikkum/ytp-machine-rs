//! A project's clips as something the sentence renderer can play: one run of
//! segments end to end (gaps filled with black and silence), plus audio clips
//! to lay over it.

use std::path::{Path, PathBuf};

use ytp_core::{Clip, ClipSource, Effect, Project, Track, TrackKind};
use ytp_splice::{segment_of_clip, Locator, Segment};

use crate::overlay::{self, Overlay};
use crate::Error;

/// Gaps shorter than half a frame are rounding, not silence to fill.
const FRAME: f64 = 1.0 / 25.0;
/// The black/silent file gaps are cut from, seconds.
const FILLER_SECONDS: f64 = 12.0;
const FILLER_PIECE: f64 = 10.0;
/// A clip this long is shortened to the window when its effects allow.
const LONG_CLIP: f64 = 6.0;

/// Where a render finds things.
pub struct Env<'a> {
    pub renderer: &'a ytp_media::FfmpegRenderer,
    pub locator: &'a Locator,
    /// Uploaded assets (`Media` sources) live here as `<asset>.<ext>`.
    pub media_dir: Option<&'a Path>,
    /// Scratch space for the filler, synthesised clips and intermediate files.
    pub work_dir: &'a Path,
}

pub struct Sequence {
    pub segments: Vec<Segment>,
    /// Audio clips to lay over the result (starting at `base_t0`).
    pub overlays: Vec<Overlay>,
    /// The project time the first kept frame sits at.
    pub base_t0: f64,
    /// Seconds to drop from the start of the sequence's render.
    pub skip: f64,
    /// Seconds to keep after `skip`.
    pub length: f64,
}

impl Env<'_> {
    /// The file of uploaded asset `asset`.
    pub fn media_path(&self, asset: &str) -> Option<PathBuf> {
        let dir = self.media_dir?;
        std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.is_file() && (p.file_stem().is_some_and(|s| s == asset) || p.file_name().is_some_and(|s| s == asset)))
    }

    fn filler(&self) -> Result<PathBuf, Error> {
        std::fs::create_dir_all(self.work_dir).map_err(|e| Error::Failed(format!("cannot make {}: {e}", self.work_dir.display())))?;
        let path = self.work_dir.join(format!("filler-{FILLER_SECONDS}s.mp4"));
        if path.metadata().is_ok_and(|m| m.len() > 0) {
            return Ok(path);
        }
        let tmp = self.work_dir.join(format!("filler-{FILLER_SECONDS}s.{}.tmp.mp4", std::process::id()));
        let mut cmd = std::process::Command::new(&self.renderer.tools().ffmpeg);
        cmd.args(["-y", "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
            .arg(format!("color=c=black:s=480x270:r=25:d={FILLER_SECONDS}"))
            .args(["-f", "lavfi", "-i", "anullsrc=r=44100:cl=stereo", "-t"])
            .arg(FILLER_SECONDS.to_string())
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-c:a", "aac", "-movflags", "+faststart"])
            .arg(&tmp);
        let out = ytp_media::proc::run(&mut cmd, std::time::Duration::from_secs(60), &|| true)
            .map_err(|e| Error::Unavailable(format!("{e:?}")))?;
        if !out.success {
            return Err(Error::Failed(format!("cannot make the filler clip: {}", ytp_media::proc::trim_stderr(&out.stderr, 500))));
        }
        std::fs::rename(&tmp, &path).map_err(|e| Error::Failed(e.to_string()))?;
        Ok(path)
    }
}

fn db_of(track: &Track, project: &Project) -> f64 {
    if !project.audible(track) || track.volume <= 0.0 {
        -200.0
    } else if (track.volume - 1.0).abs() > 1e-9 {
        20.0 * track.volume.log10()
    } else {
        0.0
    }
}

/// Source file, `in` and `out` of a clip that plays media.
fn media_of(env: &Env, clip: &Clip) -> Result<(PathBuf, f64, f64), Error> {
    match &clip.source {
        ClipSource::CorpusClip { corpus, file, in_, out, .. } => {
            let Some(file) = file else {
                return Err(Error::Invalid(format!("clip {} has no media file to play", clip.id)));
            };
            let path = env
                .locator
                .resolve(corpus, file)
                .ok_or_else(|| Error::Invalid(format!("clip {}: corpus {corpus:?} or its file {file:?} is not available", clip.id)))?;
            if !path.is_file() {
                return Err(Error::Invalid(format!("clip {}: {file:?} is missing from corpus {corpus:?}", clip.id)));
            }
            Ok((path, *in_, *out))
        }
        ClipSource::Media { asset, in_, out } => {
            let path = env.media_path(asset).ok_or_else(|| Error::Invalid(format!("clip {}: media asset {asset:?} was not found", clip.id)))?;
            Ok((path, *in_, *out))
        }
        ClipSource::Synth { .. } => Err(Error::Invalid(format!("clip {}: a synth clip has no picture", clip.id))),
        ClipSource::Generated { .. } => Err(Error::Invalid(format!(
            "clip {}: a generated clip has to be expanded into clips before it can be rendered",
            clip.id
        ))),
    }
}

/// Effects that only scale time linearly, so a window into the clip maps
/// straight onto a window into its source.
fn linear(clip: &Clip) -> bool {
    clip.effects.iter().all(|e| matches!(e, Effect::Speed { .. } | Effect::Volume { .. } | Effect::Pitch { .. } | Effect::Mirror))
        && !clip.effects.iter().any(|e| matches!(e, Effect::Glitch { .. }))
}

struct VItem<'a> {
    start: f64,
    dur: f64,
    seg: Segment,
    clip: &'a Clip,
}

/// Fillers covering `gap` seconds.
fn fillers(path: &Path, mut gap: f64) -> Vec<Segment> {
    let mut out = Vec::new();
    while gap > 1e-6 {
        let piece = gap.min(FILLER_PIECE);
        out.push(Segment {
            source_file: path.to_string_lossy().into_owned(),
            start_time: 0.0,
            end_time: piece,
            edited: true,
            length: Some(piece),
            speed: Some(1.0),
            fade_in: Some(0.0),
            fade_out: Some(0.0),
            ..Default::default()
        });
        gap -= piece;
    }
    out
}

/// Lay the project out for the sequence renderer. `window` is `(start, end)`
/// in project seconds; `None` plans the whole project.
pub fn plan(env: &Env, project: &Project, window: Option<(f64, f64)>) -> Result<Sequence, Error> {
    if project.tracks.iter().any(|t| t.generated) {
        return Err(Error::Invalid("this project is played by its generator, not as a sequence".into()));
    }
    let total = project.duration();
    if total <= 0.0 {
        return Err(Error::Invalid("the project has no clips".into()));
    }
    let (w0, w1) = match window {
        Some((a, b)) => (a.max(0.0), b.min(total)),
        None => (0.0, total),
    };
    if w1 - w0 <= 1e-6 {
        return Err(Error::Invalid("the window is empty or after the end of the project".into()));
    }

    // Video clips of every video track, as one run.
    let mut items: Vec<VItem> = Vec::new();
    for track in project.tracks.iter().filter(|t| t.kind == TrackKind::Video) {
        let gain = db_of(track, project);
        for clip in &track.clips {
            if window.is_some() && (clip.end() <= w0 || clip.start >= w1) {
                continue;
            }
            let (path, in_, out) = media_of(env, clip)?;
            let seg = segment_of_clip(project, clip, &path.to_string_lossy(), in_, out, gain);
            items.push(VItem { start: clip.start, dur: clip.duration, seg, clip });
        }
    }
    items.sort_by(|a, b| a.start.total_cmp(&b.start));
    for w in items.windows(2) {
        if w[1].start < w[0].start + w[0].dur - FRAME / 2.0 {
            return Err(Error::Invalid(format!(
                "video clips {} and {} overlap; layering video is not supported yet",
                w[0].clip.id, w[1].clip.id
            )));
        }
    }
    if window.is_some() {
        for it in &mut items {
            if it.dur > LONG_CLIP && linear(it.clip) {
                let a = (w0 - it.start).max(0.0);
                let b = (w1 - it.start).min(it.dur);
                if b - a < it.dur - 1e-6 && b - a > 0.0 {
                    let speed = it.seg.speed.unwrap_or(1.0);
                    it.seg.start_time += a * speed;
                    it.seg.end_time = it.seg.start_time + (b - a) * speed;
                    it.seg.length = Some(b - a);
                    it.start += a;
                    it.dur = b - a;
                }
            }
        }
    }

    // The kept stretch starts at the first clip when that begins earlier.
    let base_t0 = match items.first() {
        Some(first) if first.start < w0 => first.start,
        _ => w0,
    };
    let skip = w0 - base_t0;
    let length = w1 - w0;

    let mut filler_path: Option<PathBuf> = None;
    let mut segments: Vec<Segment> = Vec::new();
    let mut cursor = base_t0;
    let mut fill = |gap: f64, segments: &mut Vec<Segment>| -> Result<(), Error> {
        if gap <= FRAME / 2.0 {
            return Ok(());
        }
        if filler_path.is_none() {
            filler_path = Some(env.filler()?);
        }
        segments.extend(fillers(filler_path.as_ref().unwrap(), gap));
        Ok(())
    };
    for it in items {
        fill(it.start - cursor, &mut segments)?;
        cursor = it.start + it.dur;
        segments.push(it.seg);
    }
    fill(w1 - cursor, &mut segments)?;

    let mut overlays = Vec::new();
    for track in project.tracks.iter().filter(|t| t.kind == TrackKind::Audio) {
        if !project.audible(track) || track.volume <= 0.0 {
            continue;
        }
        let gain = db_of(track, project);
        for clip in &track.clips {
            if clip.end() <= w0 || clip.start >= w1 {
                continue;
            }
            overlays.push(overlay::overlay_of(env, clip, gain)?);
        }
    }
    Ok(Sequence { segments, overlays, base_t0, skip, length })
}
