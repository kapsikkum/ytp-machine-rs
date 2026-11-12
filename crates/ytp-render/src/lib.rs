//! Plays a timeline [`Project`].
//!
//! There are two kinds of project:
//!
//! * a **sequence** (what a sentence plan converts to, and what the Studio
//!   edits): video clips end to end on the video tracks, audio clips laid over
//!   them. [`render_full`] gives it to the sentence renderer — the same graph
//!   builder and shared encoder the YTP job uses — so a converted plan comes
//!   out as the video it was planned as; [`render_preview`] renders a time
//!   window of it quickly at low quality.
//! * a **song** (`generated` tracks from a YTPMV plan): the tracks are a
//!   readout of the song renderer, so it is played by running that renderer
//!   again with each track's mute/solo/volume applied to its part
//!   ([`song_request`]). The server drives that; this crate only classifies.

pub mod overlay;
pub mod proxy;
pub mod sequence;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use ytp_core::{GeneratorKind, Project};
use ytp_media::{master_path, Quick, RenderError, RenderJob, Renderer};
use ytp_splice::{AudioCache, GenerationOptions, NoAudio};

pub use crate::proxy::ProxyCache;
pub use crate::sequence::Env;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cancelled")]
    Cancelled,
    /// The project cannot be played this way; the person can fix it.
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Failed(String),
    #[error("ffmpeg is not available: {0}")]
    Unavailable(String),
}

impl From<RenderError> for Error {
    fn from(e: RenderError) -> Self {
        match e {
            RenderError::Cancelled => Error::Cancelled,
            RenderError::Failed(m) => Error::Failed(m),
            RenderError::Unavailable(m) => Error::Unavailable(m),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Sequence,
    Song,
}

pub fn kind(project: &Project) -> Kind {
    let song = project.generator.as_ref().is_some_and(|g| g.kind == GeneratorKind::Ytpmv) && project.tracks.iter().any(|t| t.generated);
    if song {
        Kind::Song
    } else {
        Kind::Sequence
    }
}

/// The request that plays a song project: the generator's request with each
/// generated track's mute (including solo) and volume put on its part.
pub fn song_request(project: &Project) -> Result<Value, Error> {
    let Some(g) = project.generator.as_ref().filter(|g| g.kind == GeneratorKind::Ytpmv) else {
        return Err(Error::Invalid("this project was not made from a song".into()));
    };
    let mut req = g.request.clone();
    if !req.is_object() {
        return Err(Error::Invalid("the project's song request is missing".into()));
    }
    let mut parts: Vec<Value> = req["parts"].as_array().cloned().unwrap_or_default();
    for t in project.tracks.iter().filter(|t| t.generated) {
        let Some(pid) = &t.part else { continue };
        let at = match parts.iter().position(|p| p.get("id").and_then(Value::as_str) == Some(pid)) {
            Some(i) => i,
            None => {
                parts.push(json!({ "id": pid }));
                parts.len() - 1
            }
        };
        parts[at]["mute"] = json!(!project.audible(t));
        parts[at]["volume"] = json!(t.volume);
    }
    req["parts"] = Value::Array(parts);
    Ok(req)
}

/// `progress(stage, done, total)`; return `false` to cancel.
pub type ProgressFn<'a> = &'a dyn Fn(&str, usize, usize) -> bool;

pub struct FullOptions<'a> {
    /// Output height ceiling from the caller's role quota (0 = none).
    pub max_height: u32,
    pub encoder: Option<&'a ytp_encode::Encoder>,
    pub cancel: Option<&'a dyn Fn() -> bool>,
}

/// Render the whole sequence project to `out` (the master is kept beside it as
/// `<id>.master.mp4`). Returns the length of the video in seconds.
pub fn render_full(env: &Env, project: &Project, out: &Path, opts: &FullOptions, progress: ProgressFn) -> Result<f64, Error> {
    project.validate().map_err(Error::Invalid)?;
    let seq = sequence::plan(env, project, None)?;
    let keep_going = || !opts.cancel.is_some_and(|c| c()) && progress("encoding", 0, 0);
    let audio = AudioCache::new(&NoAudio);
    let gen = GenerationOptions::default();

    if seq.overlays.is_empty() {
        let job = RenderJob {
            segments: &seq.segments,
            options: &gen,
            captions: project.captions,
            out,
            audio: &audio,
            max_height: opts.max_height,
            encoder: opts.encoder,
            finish: true,
            cancel: opts.cancel,
        };
        env.renderer.render(&job, progress)?;
        return Ok(seq.length);
    }

    // Picture and its own sound first, then the audio clips on top, then the
    // shared encode like any other render.
    let video = out.with_extension("seq.mp4");
    let job = RenderJob {
        segments: &seq.segments,
        options: &gen,
        captions: project.captions,
        out: &video,
        audio: &audio,
        max_height: 0,
        encoder: None,
        finish: false,
        cancel: opts.cancel,
    };
    let progress_in = |stage: &str, done: usize, total: usize| progress(stage, done, total);
    env.renderer.render(&job, &progress_in)?;
    let master = master_path(out);
    let res = overlay::mix_overlays(env, &video, seq.base_t0, &seq.overlays, &master, &keep_going);
    let _ = std::fs::remove_file(&video);
    res?;
    finish_master(env, opts, &master, out, progress)?;
    Ok(seq.length)
}

/// Turn the mixed master into the result with `web-default`.
fn finish_master(env: &Env, opts: &FullOptions, master: &Path, out: &Path, progress: ProgressFn) -> Result<(), Error> {
    let own;
    let encoder = match opts.encoder {
        Some(e) => e,
        None => {
            own = ytp_encode::Encoder::new(&env.renderer.tools().ffmpeg);
            &own
        }
    };
    let Some(preset) = encoder.presets().get("web-default") else {
        return Err(Error::Failed("the web-default preset is missing".into()));
    };
    let last = AtomicUsize::new(0);
    let report = |f: f32| {
        let done = (f.clamp(0.0, 1.0) * 100.0) as usize;
        last.store(done, Ordering::Relaxed);
        progress("finishing", done, 100);
    };
    let cancel = || opts.cancel.is_some_and(|c| c()) || !progress("finishing", last.load(Ordering::Relaxed), 100);
    let overrides = ytp_encode::Overrides { max_width: 0, max_height: opts.max_height };
    match encoder.encode(master, preset, overrides, out, &report, &cancel) {
        Ok(_) => Ok(()),
        Err(ytp_encode::EncodeError::Cancelled) => {
            let _ = std::fs::remove_file(master);
            Err(Error::Cancelled)
        }
        Err(e) => {
            let _ = std::fs::remove_file(master);
            let _ = std::fs::remove_file(out);
            Err(Error::Failed(e.to_string()))
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreviewInfo {
    pub start: f64,
    pub duration: f64,
    pub segments: usize,
    pub overlays: usize,
    /// Segments read from a ready proxy.
    pub proxied: usize,
}

/// CRF of the preview encode: small and quick, 480p is already tiny.
const PREVIEW_CRF: u32 = 30;

/// Render `start..start + duration` of a sequence project to `out` at low
/// quality (480p H.264 + AAC, one fast ffmpeg call). The window is cut to the
/// end of the project.
pub fn render_preview(
    env: &Env,
    project: &Project,
    start: f64,
    duration: f64,
    out: &Path,
    proxy: Option<&Arc<ProxyCache>>,
    keep_going: &dyn Fn() -> bool,
) -> Result<PreviewInfo, Error> {
    if !(start.is_finite() && duration.is_finite() && start >= 0.0 && duration > 0.0) {
        return Err(Error::Invalid("a preview needs start >= 0 and duration > 0".into()));
    }
    let mut seq = sequence::plan(env, project, Some((start, start + duration)))?;
    let mut proxied = 0;
    if let Some(cache) = proxy {
        for seg in &mut seq.segments {
            if let Some(p) = cache.lookup(&seg.source_file) {
                seg.source_file = p;
                proxied += 1;
            }
        }
    }
    let audio = AudioCache::new(&NoAudio);
    let gen = GenerationOptions::default();
    let quick = Quick { skip: seq.skip, length: seq.length, crf: PREVIEW_CRF };
    let progress = |_: &str, _: usize, _: usize| keep_going();
    let base = if seq.overlays.is_empty() { out.to_path_buf() } else { out.with_extension("base.mp4") };
    env.renderer.render_quick(&seq.segments, &gen, project.captions, &audio, &base, &quick, &progress)?;
    if !seq.overlays.is_empty() {
        // The kept stretch starts at the project time `start`.
        let res = overlay::mix_overlays(env, &base, start, &seq.overlays, out, keep_going);
        let _ = std::fs::remove_file(&base);
        res?;
    }
    Ok(PreviewInfo { start, duration: seq.length, segments: seq.segments.len(), overlays: seq.overlays.len(), proxied })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ytp_core::{Clip, ClipSource, Generator, Track, TrackKind};

    fn song_project() -> Project {
        let mut p = Project::new("song");
        p.generator = Some(Generator {
            kind: GeneratorKind::Ytpmv,
            request: json!({ "midi_id": "abc", "parts": [{ "id": "p1", "text": "ah", "volume": 0.5 }], "options": {} }),
            seed: String::new(),
        });
        for (id, part) in [("p1", "p1"), ("p2", "p2")] {
            let mut t = Track::new(id, TrackKind::Audio, id);
            t.generated = true;
            t.part = Some(part.into());
            t.volume = 0.8;
            t.clips.push(Clip {
                id: format!("{id}-0"),
                start: 0.0,
                duration: 1.0,
                source: ClipSource::Synth { chip: "nes".into(), notes: vec![] },
                effects: vec![],
                label: None,
                caption: None,
            });
            p.tracks.push(t);
        }
        p
    }

    #[test]
    fn songs_are_told_apart_and_carry_the_track_mixer_to_their_parts() {
        let mut p = song_project();
        assert_eq!(kind(&p), Kind::Song);
        p.tracks[0].mute = true;
        let req = song_request(&p).unwrap();
        let parts = req["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["id"], "p1");
        assert_eq!(parts[0]["text"], "ah", "the part's own settings stay");
        assert_eq!(parts[0]["mute"], true);
        assert_eq!(parts[0]["volume"], 0.8);
        assert_eq!(parts[1]["mute"], false);
        assert_eq!(req["midi_id"], "abc");
        // Solo silences the others.
        p.tracks[0].mute = false;
        p.tracks[1].solo = true;
        let req = song_request(&p).unwrap();
        assert_eq!(req["parts"][0]["mute"], true);
        assert_eq!(req["parts"][1]["mute"], false);
        // A sentence project is a sequence.
        assert_eq!(kind(&Project::new("x")), Kind::Sequence);
        assert!(song_request(&Project::new("x")).is_err());
    }
}
