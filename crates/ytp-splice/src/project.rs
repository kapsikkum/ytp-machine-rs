//! A sentence plan as a timeline [`Project`], and back.
//!
//! [`project_from_plan`] lays the chosen segments end to end on one video
//! track: each clip is the exact span of source audio the renderer would
//! have pulled (`extract_window`, analysis already applied), its length on the
//! timeline is the planned one, and everything the segment does to it
//! (stretch, reverse, pitch, fades, glitches, captions) is an [`Effect`] or
//! caption. [`segments_from_track`] turns clips back into segments marked
//! *edited*, which the renderer cuts exactly at `in`..`out` — so a converted
//! project renders the same video as the plan it came from.

use std::path::{Path, PathBuf};

use ytp_core::{Caption, Clip, ClipSource, Effect, Generator, GeneratorKind, Project, Track, TrackKind};

use crate::audio::AudioCache;
use crate::model::Segment;
use crate::options::GenerationOptions;
use crate::window::{extract_window, segment_durations, PAD_END, PAD_END_LAST};

/// Corpus directories by name: turns a source path into `(corpus, relative
/// file)` and back.
#[derive(Debug, Clone, Default)]
pub struct Locator {
    dirs: Vec<(String, PathBuf)>,
}

impl Locator {
    pub fn new(dirs: Vec<(String, PathBuf)>) -> Self {
        Self { dirs }
    }

    /// Which corpus holds `abs`, and where inside it (forward slashes).
    pub fn locate(&self, abs: &str) -> Option<(String, String)> {
        let abs = Path::new(abs);
        let mut best: Option<(&str, String, usize)> = None;
        for (name, dir) in &self.dirs {
            if let Ok(rel) = abs.strip_prefix(dir) {
                let depth = dir.components().count();
                if best.as_ref().is_none_or(|b| depth > b.2) {
                    best = Some((name, rel.to_string_lossy().replace('\\', "/"), depth));
                }
            }
        }
        best.map(|(n, r, _)| (n.to_string(), r))
    }

    /// The file `rel` of corpus `corpus`, if it stays inside the corpus.
    pub fn resolve(&self, corpus: &str, rel: &str) -> Option<PathBuf> {
        let dir = &self.dirs.iter().find(|(n, _)| n == corpus)?.1;
        ytp_corpus::data::resolve_in(dir, rel)
    }
}

/// What a plan needs to say about itself besides its segments.
pub struct PlanInfo<'a> {
    pub name: &'a str,
    /// The main voice.
    pub voice: &'a str,
    pub text: &'a str,
    pub captions: bool,
    pub options: &'a GenerationOptions,
    /// The sentence request as the job had it (kept so the project can say how it was made).
    pub request: serde_json::Value,
}

/// The effects a segment applies, `speed` and `pitch` being the options'.
fn effects_of(seg: &Segment, opts: &GenerationOptions) -> Vec<Effect> {
    let mut fx = Vec::new();
    let speed = seg.speed.unwrap_or(opts.speed);
    if speed != 1.0 {
        fx.push(Effect::Speed { factor: speed });
    }
    if let Some(s) = seg.stretch.filter(|s| *s != 0.0 && *s != 1.0) {
        fx.push(Effect::Stretch { factor: s });
    }
    if seg.reverse {
        fx.push(Effect::Reverse);
    }
    let pitch = opts.pitch + seg.pitch.unwrap_or(0.0);
    if pitch != 0.0 {
        fx.push(Effect::Pitch { semitones: pitch });
    }
    if let Some(db) = seg.gain_db.filter(|g| *g != 0.0) {
        fx.push(Effect::Volume { db });
    }
    if let Some(s) = seg.fade_in {
        fx.push(Effect::FadeIn { seconds: s });
    }
    if let Some(s) = seg.fade_out {
        fx.push(Effect::FadeOut { seconds: s });
    }
    if seg.pause() > 0.0 {
        fx.push(Effect::Pause { seconds: seg.pause() });
    }
    if seg.subword {
        fx.push(Effect::Subword);
    }
    for g in &seg.glitch {
        fx.push(Effect::from_glitch(g));
    }
    fx
}

/// Window edges are multiples of 10 ms plus float noise; the noise is dropped.
fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

/// The plan as a project with one video track.
pub fn project_from_plan(
    segments: &[Segment],
    audio: &AudioCache,
    info: &PlanInfo,
    locator: &Locator,
) -> Result<Project, String> {
    let opts = info.options;
    let mut project = Project::new(info.name);
    project.corpus = info.voice.to_string();
    project.captions = info.captions;
    project.generator = Some(Generator { kind: GeneratorKind::Ytp, request: info.request.clone(), seed: opts.seed.clone() });
    let mut track = Track::new("video", TrackKind::Video, "Video");
    let mut t = 0.0;
    for (i, seg) in segments.iter().enumerate() {
        let pad = if i + 1 == segments.len() { PAD_END_LAST } else { PAD_END };
        let win = extract_window(seg, pad, audio);
        let (_span, _sounding, total) = segment_durations(seg, win, opts);
        let Some((corpus, file)) = locator.locate(&seg.source_file) else {
            return Err(format!("\"{}\" comes from a file outside the installed corpora", seg.word));
        };
        let caption = if !seg.subtitle_words.is_empty() {
            Some(Caption { text: seg.subtitle.clone().unwrap_or_default(), words: seg.subtitle_words.clone() })
        } else {
            seg.subtitle.as_ref().map(|s| Caption { text: s.clone(), words: Vec::new() })
        };
        let duration = if total > 0.0 { total } else { 1.0 / 25.0 };
        track.clips.push(Clip {
            id: format!("c{i}"),
            start: t,
            duration,
            source: ClipSource::CorpusClip {
                corpus,
                clip_id: seg.id,
                file: Some(file),
                word: (!seg.word.is_empty()).then(|| seg.word.clone()),
                take: None,
                in_: round6(win.0),
                out: round6(win.1.max(win.0 + 0.001)),
            },
            effects: effects_of(seg, opts),
            label: (!seg.word.is_empty()).then(|| seg.word.clone()),
            caption,
        });
        t += duration;
    }
    project.tracks.push(track);
    let _ = info.text;
    Ok(project)
}

/// One video track's clips as segments, in order of `start`.
///
/// `gain_db` is added to every clip (the track's volume, or silence when it
/// is not heard). Gaps between clips are the caller's to fill. Captions are
/// kept only when the project asks for them.
pub fn segments_from_track(
    project: &Project,
    track: &Track,
    gain_db: f64,
    locator: &Locator,
) -> Result<Vec<(f64, Segment)>, String> {
    let mut clips: Vec<&Clip> = track.clips.iter().collect();
    clips.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut out = Vec::with_capacity(clips.len());
    for clip in clips {
        let ClipSource::CorpusClip { corpus, file, in_, out: end, .. } = &clip.source else {
            return Err(format!("clip {}: only corpus clips can be played in a sequence", clip.id));
        };
        let Some(file) = file else {
            return Err(format!("clip {}: it has no media file", clip.id));
        };
        let Some(path) = locator.resolve(corpus, file) else {
            return Err(format!("clip {}: corpus {corpus:?} or its file {file:?} is not available", clip.id));
        };
        out.push((clip.start, segment_of_clip(project, clip, &path.to_string_lossy(), *in_, *end, gain_db)));
    }
    Ok(out)
}

/// The segment that plays `clip`'s effects over `in_..out` of `source_file`:
/// marked *edited*, so the renderer cuts exactly that span, and carrying the
/// clip's own length. `gain_db` is added to the clip's volume effects.
pub fn segment_of_clip(project: &Project, clip: &Clip, source_file: &str, in_: f64, out: f64, gain_db: f64) -> Segment {
    let (id, word) = match &clip.source {
        ClipSource::CorpusClip { clip_id, word, .. } => (*clip_id, word.clone()),
        _ => (None, None),
    };
    let mut seg = Segment {
        id,
        word: word.or_else(|| clip.label.clone()).unwrap_or_default(),
        start_time: in_,
        end_time: out,
        source_file: source_file.to_string(),
        edited: true,
        length: Some(clip.duration),
        speed: Some(1.0),
        ..Default::default()
    };
    let mut gain = gain_db;
    let mut pitch = 0.0;
    for e in &clip.effects {
        match e {
            Effect::Speed { factor } => seg.speed = Some(*factor),
            Effect::Stretch { factor } => seg.stretch = Some(*factor),
            Effect::Reverse => seg.reverse = true,
            Effect::Pitch { semitones } => pitch += semitones,
            Effect::Volume { db } => gain += db,
            Effect::FadeIn { seconds } => seg.fade_in = Some(*seconds),
            Effect::FadeOut { seconds } => seg.fade_out = Some(*seconds),
            Effect::Pause { seconds } => seg.pause_after = Some(*seconds),
            Effect::Subword => seg.subword = true,
            other => {
                if let Some(g) = other.glitch_name() {
                    seg.glitch.push(g.to_string());
                }
            }
        }
    }
    if pitch != 0.0 {
        seg.pitch = Some(pitch);
    }
    if gain != 0.0 {
        seg.gain_db = Some(gain);
    }
    if project.captions {
        if let Some(c) = &clip.caption {
            if !c.words.is_empty() {
                seg.subtitle_words = c.words.clone();
            }
            seg.subtitle = Some(c.text.clone()).filter(|t| !t.is_empty());
        }
    }
    seg
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::NoAudio;

    fn seg(word: &str, a: f64, b: f64, file: &str) -> Segment {
        Segment {
            id: Some(7),
            word: word.into(),
            start_time: a,
            end_time: b,
            source_file: file.into(),
            prev_end: Some(a - 0.2),
            next_start: Some(b + 0.2),
            subtitle: Some(word.into()),
            tok: Some(0),
            pause_after: Some(0.03),
            ..Default::default()
        }
    }

    fn locator() -> Locator {
        let base = std::env::temp_dir().join("ytp-splice-project-test");
        Locator::new(vec![("voice".into(), base)])
    }

    fn info<'a>(opts: &'a GenerationOptions) -> PlanInfo<'a> {
        PlanInfo { name: "t", voice: "voice", text: "hi", captions: true, options: opts, request: serde_json::json!({}) }
    }

    #[test]
    fn a_plan_becomes_clips_laid_end_to_end() {
        let loc = locator();
        let file = loc.resolve("voice", "downloads/v.mp4").unwrap().to_string_lossy().into_owned();
        let mut a = seg("hello", 1.0, 1.4, &file);
        a.stretch = Some(2.0);
        a.reverse = true;
        a.pitch = Some(3.0);
        a.glitch = vec!["flip".into(), "shake".into()];
        a.fade_in = Some(0.0);
        let b = seg("world", 3.0, 3.5, &file);
        let opts = GenerationOptions { speed: 1.25, pitch: 1.0, ..Default::default() };
        let audio = AudioCache::new(&NoAudio);
        let p = project_from_plan(&[a.clone(), b.clone()], &audio, &info(&opts), &loc).unwrap();
        p.validate().unwrap();
        assert_eq!(p.tracks.len(), 1);
        let clips = &p.tracks[0].clips;
        assert_eq!(clips.len(), 2);
        // laid end to end, durations as planned
        let wa = extract_window(&a, PAD_END, &audio);
        let da = segment_durations(&a, wa, &opts).2;
        assert!((clips[0].duration - da).abs() < 1e-9);
        assert!((clips[1].start - da).abs() < 1e-9);
        let ClipSource::CorpusClip { in_, out, file, corpus, clip_id, .. } = &clips[0].source else { panic!() };
        assert!((in_ - wa.0).abs() < 1e-6 && (out - wa.1).abs() < 1e-6, "{in_} {out} {wa:?}");
        assert_eq!((corpus.as_str(), file.as_deref(), *clip_id), ("voice", Some("downloads/v.mp4"), Some(7)));
        let fx = &clips[0].effects;
        assert!(fx.contains(&Effect::Speed { factor: 1.25 }));
        assert!(fx.contains(&Effect::Stretch { factor: 2.0 }));
        assert!(fx.contains(&Effect::Reverse));
        assert!(fx.contains(&Effect::Pitch { semitones: 4.0 }));
        assert!(fx.contains(&Effect::Mirror));
        assert!(fx.contains(&Effect::Glitch { name: "shake".into() }));
        assert!(fx.contains(&Effect::FadeIn { seconds: 0.0 }));
        assert!(fx.contains(&Effect::Pause { seconds: 0.03 }));
        assert_eq!(clips[0].caption.as_ref().unwrap().text, "hello");
    }

    #[test]
    fn a_source_outside_the_corpora_is_refused() {
        let opts = GenerationOptions::default();
        let audio = AudioCache::new(&NoAudio);
        let err = project_from_plan(&[seg("x", 0.0, 0.3, "/elsewhere/x.mp4")], &audio, &info(&opts), &locator()).unwrap_err();
        assert!(err.contains("outside"));
    }

    #[test]
    fn clips_read_back_as_the_same_cut_with_the_same_lengths() {
        let loc = locator();
        let file = loc.resolve("voice", "v.mp4").unwrap().to_string_lossy().into_owned();
        let mut a = seg("hello", 1.0, 1.4, &file);
        a.stretch = Some(1.5);
        a.gain_db = Some(6.0);
        a.glitch = vec!["stutter".into()];
        a.subword = true;
        let b = seg("world", 3.0, 3.5, &file);
        let opts = GenerationOptions::from_json(&serde_json::json!({ "speed": 1.2, "pitch": 2 }));
        let audio = AudioCache::new(&NoAudio);
        let p = project_from_plan(&[a.clone(), b.clone()], &audio, &info(&opts), &loc).unwrap();
        let back = segments_from_track(&p, &p.tracks[0], 0.0, &loc).unwrap();
        assert_eq!(back.len(), 2);
        // The edited segment's window is exactly the planned one (the last
        // clip of a plan carries the longer tail pad).
        for (k, ((_, s), orig)) in back.iter().zip([&a, &b]).enumerate() {
            let pad = if k == 1 { PAD_END_LAST } else { PAD_END };
            let (got, want) = (extract_window(s, PAD_END, &audio), extract_window(orig, pad, &audio));
            assert!((got.0 - want.0).abs() < 1e-6 && (got.1 - want.1).abs() < 1e-6, "{got:?} {want:?}");
            assert_eq!(s.source_file, file);
            assert!(s.edited && s.prev_end.is_none() && s.next_start.is_none());
        }
        let (_, s0) = &back[0];
        assert_eq!(s0.stretch, Some(1.5));
        assert_eq!(s0.speed, Some(1.2));
        assert_eq!(s0.pitch, Some(2.0));
        assert_eq!(s0.gain_db, Some(6.0));
        assert_eq!(s0.glitch, vec!["stutter".to_string()]);
        assert!(s0.subword);
        // ...and the length is the clip's, whatever the formula would give.
        let w = (s0.start_time, s0.end_time);
        assert_eq!(segment_durations(s0, w, &GenerationOptions::default()).2, p.tracks[0].clips[0].duration);
    }

    #[test]
    fn track_gain_and_captions_follow_the_project() {
        let loc = locator();
        let file = loc.resolve("voice", "v.mp4").unwrap().to_string_lossy().into_owned();
        let opts = GenerationOptions::default();
        let audio = AudioCache::new(&NoAudio);
        let mut p = project_from_plan(&[seg("hi", 1.0, 1.3, &file)], &audio, &info(&opts), &loc).unwrap();
        let s = &segments_from_track(&p, &p.tracks[0], -6.0, &loc).unwrap()[0].1;
        assert_eq!(s.gain_db, Some(-6.0));
        assert_eq!(s.subtitle.as_deref(), Some("hi"));
        p.captions = false;
        let s = &segments_from_track(&p, &p.tracks[0], 0.0, &loc).unwrap()[0].1;
        assert_eq!(s.subtitle, None);
        assert_eq!(s.gain_db, None);
    }
}
