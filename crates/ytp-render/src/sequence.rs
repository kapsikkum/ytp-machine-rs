//! A project's clips as something the sentence renderer can play: one run of
//! segments end to end (gaps filled with black and silence), plus audio clips
//! to lay over it.
//!
//! Clips of *different* video tracks may run at the same time: the later track
//! is composited over the earlier one as a picture-in-picture. Clips of one
//! track still have to follow each other (see [`spans_of`] and [`composite`]).
//!
//! Captions are burned into each layer's own intermediate file *before* the
//! layers are composited, with the same renderer and the same graph the
//! ordinary path uses. Scaling or placing a layer therefore carries its text
//! along, and every layer that has a caption visible at that moment shows it.
//! Where two layers' captions are on screen at once they simply overlap: the
//! upper layer's text is drawn over the lower one's, nothing is merged or
//! suppressed.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ytp_core::{Clip, ClipSource, Effect, Place, Project, Track, TrackKind};
use ytp_media::{RenderJob, Renderer};
use ytp_splice::{segment_of_clip, AudioCache, GenerationOptions, Locator, NoAudio, Segment};

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
    /// The scratch files this plan made. A composite is read by the render
    /// that plays the plan, so it has to live as long as the plan does and is
    /// removed with it.
    #[allow(dead_code)] // held only so its `Drop` runs with the plan
    made: Made,
}

/// Scratch files a plan owns, removed when the plan is. Only files this plan
/// created are listed, so another render sharing the work directory never
/// loses one of its own.
#[derive(Default)]
struct Made {
    files: Vec<PathBuf>,
}

impl Made {
    fn keep(&mut self, path: &Path) {
        self.files.push(path.to_path_buf());
    }
}

impl Drop for Made {
    fn drop(&mut self) {
        for f in &self.files {
            let _ = std::fs::remove_file(f);
        }
    }
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
    /// Index of the clip's track in the project: later tracks paint on top.
    track: usize,
    start: f64,
    dur: f64,
    seg: Segment,
    clip: &'a Clip,
}

/// A stretch of the run: a clip, a piece of one, or a composite of several.
struct Piece<'a> {
    start: f64,
    dur: f64,
    seg: Segment,
    /// The clip when this piece is the whole of one, so a window may shorten
    /// it rather than decode it all.
    clip: Option<&'a Clip>,
    /// A file this plan made (a composite), so a window may cut into it.
    own: bool,
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

/// The stretches where two or more video tracks play at once, in order, each
/// with the clips playing there (bottom track first). Consecutive stretches with
/// the same clips are one span.
fn spans_of(items: &[VItem]) -> Vec<((f64, f64), Vec<usize>)> {
    let mut edges: Vec<f64> = items.iter().flat_map(|i| [i.start, i.start + i.dur]).collect();
    edges.sort_by(f64::total_cmp);
    edges.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    let mut spans: Vec<((f64, f64), Vec<usize>)> = Vec::new();
    for w in edges.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b - a <= 1e-9 {
            continue;
        }
        let mut active: Vec<usize> = (0..items.len())
            .filter(|&k| {
                let it = &items[k];
                it.start <= a + 1e-9 && it.start + it.dur >= b - 1e-9
            })
            .collect();
        if active.len() < 2 {
            continue;
        }
        active.sort_by_key(|&k| items[k].track);
        match spans.last_mut() {
            Some((last, set)) if *set == active && (last.1 - a).abs() < 1e-9 => last.1 = b,
            _ => spans.push(((a, b), active)),
        }
    }
    spans
}

/// The run with each overlapping stretch replaced by a composite and every
/// clip cut around the stretches that cover it.
fn layered<'a>(
    env: &Env,
    items: &[VItem<'a>],
    spans: &[((f64, f64), Vec<usize>)],
    frame: (u32, u32),
    captions: bool,
    made: &mut Made,
) -> Result<Vec<Piece<'a>>, Error> {
    let mut pieces: Vec<Piece<'a>> = Vec::new();
    for ((a, b), set) in spans {
        let layers: Vec<&VItem> = set.iter().map(|&k| &items[k]).collect();
        let seg = composite(env, *a, b - a, &layers, frame, captions, made)?;
        pieces.push(Piece { start: *a, dur: b - a, seg, clip: None, own: true });
    }
    for (k, it) in items.iter().enumerate() {
        let mut edges = vec![it.start, it.start + it.dur];
        for ((a, b), set) in spans {
            if set.contains(&k) {
                edges.push(*a);
                edges.push(*b);
            }
        }
        edges.sort_by(f64::total_cmp);
        edges.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        for w in edges.windows(2) {
            let (a, b) = (w[0], w[1]);
            if b - a <= FRAME / 2.0 {
                continue;
            }
            // The stretch itself is played by the composite.
            if spans.iter().any(|((x, y), set)| set.contains(&k) && a >= *x - 1e-9 && b <= *y + 1e-9) {
                continue;
            }
            pieces.push(piece_of(env, it, a, b, frame, captions, made)?);
        }
    }
    Ok(pieces)
}

/// `[start, end)` of clip `it` as a piece of the run.
fn piece_of<'a>(env: &Env, it: &VItem<'a>, a: f64, b: f64, frame: (u32, u32), captions: bool, made: &mut Made) -> Result<Piece<'a>, Error> {
    if a <= it.start + 1e-9 && b >= it.start + it.dur - 1e-9 {
        return Ok(Piece { start: it.start, dur: it.dur, seg: it.seg.clone(), clip: Some(it.clip), own: false });
    }
    if linear(it.clip) {
        // A window straight into the source: no need to decode the rest.
        let mut seg = it.seg.clone();
        let speed = seg.speed.unwrap_or(1.0);
        seg.start_time += (a - it.start).max(0.0) * speed;
        seg.end_time = seg.start_time + (b - a) * speed;
        seg.length = Some(b - a);
        return Ok(Piece { start: a, dur: b - a, seg, clip: None, own: false });
    }
    let seg = composite(env, a, b - a, &[it], frame, captions, made)?;
    Ok(Piece { start: a, dur: b - a, seg, clip: None, own: true })
}

/// Play `start..start + dur` of every layer as one file: the first layer is the
/// picture, each later track is laid over it, scaled to half the width and put
/// at the bottom right, unless it has a [`Place`] of its own. Every layer's
/// sound is mixed, so the ones above are heard too.
///
/// `captions` burns each layer's own caption into that layer's file before it
/// is scaled and overlaid, so the text travels with its picture.
fn composite(env: &Env, start: f64, dur: f64, layers: &[&VItem], frame: (u32, u32), captions: bool, made: &mut Made) -> Result<Segment, Error> {
    std::fs::create_dir_all(env.work_dir).map_err(|e| Error::Failed(format!("cannot make {}: {e}", env.work_dir.display())))?;
    let options = GenerationOptions::default();
    let audio = AudioCache::new(&NoAudio);
    let mut files: Vec<PathBuf> = Vec::new();
    let mut offsets: Vec<f64> = Vec::new();
    for (k, it) in layers.iter().enumerate() {
        let d0 = (start - it.start).max(0.0);
        let mut seg = it.seg.clone();
        let offset = if linear(it.clip) {
            // A straight cut of the source: read just the span.
            let speed = seg.speed.unwrap_or(1.0);
            seg.start_time += d0 * speed;
            seg.end_time = seg.start_time + dur * speed;
            seg.length = Some(dur);
            0.0
        } else {
            // Not a straight cut: render the clip and trim it back here.
            d0
        };
        let path = scratch(env, &format!("pip-{k}"));
        // Removed right after the mux below; `made` is only the backstop if a
        // layer render or the mux fails part-way.
        made.keep(&path);
        let job = RenderJob {
            segments: std::slice::from_ref(&seg),
            options: &options,
            captions,
            out: &path,
            audio: &audio,
            max_height: 0,
            encoder: None,
            finish: false,
            cancel: None,
        };
        env.renderer.render(&job, &|_, _, _| true)?;
        files.push(path);
        offsets.push(offset);
    }
    let path = scratch(env, "pip");
    made.keep(&path);
    let places: Vec<Option<Place>> = layers.iter().map(|it| it.clip.place.clone()).collect();
    let done = overlay_layers(env, &files, &offsets, &places, dur, frame, &path);
    // The layer files are only inputs to the mux above, which has returned:
    // free them now rather than holding several full renders on disk. The
    // composite `path` is the plan's own file and goes with the plan.
    for f in &files {
        let _ = std::fs::remove_file(f);
    }
    done?;
    Ok(Segment {
        source_file: path.to_string_lossy().into_owned(),
        start_time: 0.0,
        end_time: dur,
        edited: true,
        length: Some(dur),
        speed: Some(1.0),
        fade_in: Some(0.0),
        fade_out: Some(0.0),
        ..Default::default()
    })
}

/// One ffmpeg call: trim each layer to the span and overlay the ones above the
/// bottom over what is under them. A layer with a [`Place`] is scaled to that
/// fraction of the frame and put at its corner; one without keeps the default
/// box at the bottom right, half the width. Every layer's sound is kept and
/// mixed together.
fn overlay_layers(
    env: &Env,
    files: &[PathBuf],
    offsets: &[f64],
    places: &[Option<Place>],
    length: f64,
    frame: (u32, u32),
    out: &Path,
) -> Result<(), Error> {
    let n = files.len();
    let cut = |k: usize| format!("trim=start={:.4}:end={:.4}", offsets[k], offsets[k] + length);
    let sound = |k: usize| format!("[{k}:a]atrim=start={:.4}:end={:.4},asetpts=PTS-STARTPTS", offsets[k], offsets[k] + length);
    let mut graph: Vec<String> = vec![format!("[0:v]{},setpts=PTS-STARTPTS[b0]", cut(0))];
    if n == 1 {
        // Nothing above the bottom layer: its sound goes through untouched.
        graph.push(format!("{}[aout]", sound(0)));
    } else {
        // The bottom layer keeps its full level; every upper layer is added on
        // top of it. `normalize=0` (and `dropout_transition=0`) means silence
        // in one layer never turns the others down, and a layer that ends early
        // does not pump the mix. The limiter only catches the sum peaking over
        // full scale; `level=false` keeps it from lifting quiet material.
        for k in 0..n {
            graph.push(format!("{}[a{k}]", sound(k)));
        }
        let inputs: String = (0..n).map(|k| format!("[a{k}]")).collect();
        graph.push(format!(
            "{inputs}amix=inputs={n}:duration=longest:dropout_transition=0:normalize=0,alimiter=limit=0.95:level=false[aout]"
        ));
    }
    for k in 1..n {
        match places.get(k).and_then(|p| p.as_ref()) {
            // A placed layer: its own box, in fractions of the frame. The
            // scale is even (yuv420p needs that) and never zero, so a third of
            // a frame stays a third and a sliver stays visible.
            Some(place) => {
                let (fx, fy) = (frac(place.x), frac(place.y));
                let (fw, fh) = (frac(place.w), frac(place.h));
                graph.push(format!(
                    "[{k}:v]{},setpts=PTS-STARTPTS,scale=w='max(2,trunc(iw*{fw}/2)*2)':h='max(2,trunc(ih*{fh}/2)*2)'[t{k}]",
                    cut(k)
                ));
                graph.push(format!(
                    "[b{}][t{k}]overlay=x='round({fx}*{})':y='round({fy}*{})'[b{k}]",
                    k - 1,
                    frame.0,
                    frame.1
                ));
            }
            // No place: the default box. Every layer is the renderer's own
            // frame, so iw/2 is half the picture and main_w is the frame.
            None => {
                graph.push(format!("[{k}:v]{},setpts=PTS-STARTPTS,scale=w=trunc(iw/2/2)*2:h=-2[t{k}]", cut(k)));
                graph.push(format!("[b{}][t{k}]overlay=x=main_w-overlay_w:y=main_h-overlay_h[b{k}]", k - 1));
            }
        }
    }
    graph.push(format!("[b{}]format=yuv420p[vout]", n - 1));

    let mut cmd = std::process::Command::new(&env.renderer.tools().ffmpeg);
    cmd.args(["-y", "-hide_banner", "-loglevel", "error"]);
    for f in files {
        cmd.arg("-i").arg(f);
    }
    cmd.arg("-filter_complex").arg(graph.join(";"));
    cmd.args(["-map", "[vout]", "-map", "[aout]", "-t"]).arg(format!("{length:.4}"));
    cmd.args([
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        "-ar",
        "44100",
        "-ac",
        "2",
        "-shortest",
        "-movflags",
        "+faststart",
    ]);
    cmd.arg(out);
    let res = ytp_media::proc::run(&mut cmd, Duration::from_secs(120), &|| true).map_err(|e| Error::Unavailable(format!("{e:?}")))?;
    if !res.success {
        return Err(Error::Failed(format!(
            "cannot composite the overlapping video: {}",
            ytp_media::proc::trim_stderr(&res.stderr, 500)
        )));
    }
    Ok(())
}

static SCRATCH: AtomicU64 = AtomicU64::new(0);

/// A fraction as a filter expression number: enough digits that a third stays a
/// third to well under a pixel of a 1920-wide frame.
fn frac(v: f64) -> String {
    format!("{v:.9}")
}

/// A fresh scratch name, so two renders of a project do not collide.
fn scratch(env: &Env, tag: &str) -> PathBuf {
    let n = SCRATCH.fetch_add(1, Ordering::Relaxed);
    env.work_dir.join(format!("{tag}-{}-{n}.mp4", std::process::id()))
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
    for (ti, track) in project.tracks.iter().enumerate() {
        if track.kind != TrackKind::Video {
            continue;
        }
        let gain = db_of(track, project);
        for clip in &track.clips {
            if window.is_some() && (clip.end() <= w0 || clip.start >= w1) {
                continue;
            }
            let (path, in_, out) = media_of(env, clip)?;
            let seg = segment_of_clip(project, clip, &path.to_string_lossy(), in_, out, gain);
            items.push(VItem { track: ti, start: clip.start, dur: clip.duration, seg, clip });
        }
    }
    // Clips of one track still have to follow each other; only clips of
    // different tracks may run at the same time.
    for (ti, _) in project.tracks.iter().enumerate() {
        let mut own: Vec<&VItem> = items.iter().filter(|i| i.track == ti).collect();
        own.sort_by(|a, b| a.start.total_cmp(&b.start));
        for w in own.windows(2) {
            if w[1].start < w[0].start + w[0].dur - FRAME / 2.0 {
                return Err(Error::Invalid(format!(
                    "video clips {} and {} overlap on the same video track; put one of them on another track to layer them",
                    w[0].clip.id, w[1].clip.id
                )));
            }
        }
    }
    items.sort_by(|a, b| a.start.total_cmp(&b.start));

    // Overlaps across tracks become composites; clips are cut around them.
    let frame = (project.width, project.height);
    let spans = spans_of(&items);
    // Composites made below are the plan's own files: the render reads them
    // after `plan` returns, so nothing but the plan may remove them.
    let mut made = Made::default();
    let mut pieces: Vec<Piece> = if spans.is_empty() {
        items.into_iter().map(|it| Piece { start: it.start, dur: it.dur, seg: it.seg, clip: Some(it.clip), own: false }).collect()
    } else {
        layered(env, &items, &spans, frame, project.captions, &mut made)?
    };
    pieces.sort_by(|a, b| a.start.total_cmp(&b.start));

    if window.is_some() {
        for p in &mut pieces {
            // A long clip is cut to the window instead of decoded in full; a
            // composite is our own file, so it can always be cut.
            if p.own || (p.dur > LONG_CLIP && p.clip.is_some_and(linear)) {
                let a = (w0 - p.start).max(0.0);
                let b = (w1 - p.start).min(p.dur);
                if b - a < p.dur - 1e-6 && b - a > 0.0 {
                    let speed = p.seg.speed.unwrap_or(1.0);
                    p.seg.start_time += a * speed;
                    p.seg.end_time = p.seg.start_time + (b - a) * speed;
                    p.seg.length = Some(b - a);
                    p.start += a;
                    p.dur = b - a;
                }
            }
        }
    }

    // The kept stretch starts at the first piece when that begins earlier.
    let base_t0 = match pieces.first() {
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
    for p in pieces {
        fill(p.start - cursor, &mut segments)?;
        cursor = p.start + p.dur;
        segments.push(p.seg);
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
    Ok(Sequence { segments, overlays, base_t0, skip, length, made })
}
