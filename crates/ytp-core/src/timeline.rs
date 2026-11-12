//! The timeline model: one [`Project`] both generators produce and the
//! Studio edits. It is plain data (serde, versioned) and knows nothing about
//! rendering; `ytp-render` plays it.
//!
//! Times are seconds. A [`Clip`] sits at `start` on the project timeline for
//! `duration`, and plays the part of its source between `in` and `out`
//! (source seconds) through its [`Effect`]s. `duration` is authoritative for
//! layout: a renderer trims or pads the clip's result to it.
//!
//! Validation lives here ([`Project::validate`]) so the API, the renderer and
//! the editor all refuse the same malformed projects.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The format revision written by this build. Older/newer documents are
/// refused by [`Project::validate`] rather than half-understood.
pub const PROJECT_VERSION: u32 = 1;

pub const MAX_TRACKS: usize = 256;
pub const MAX_CLIPS: usize = 50_000;
pub const MAX_NOTES: usize = 4096;
/// Longest timeline a project may describe, seconds.
pub const MAX_SECONDS: f64 = 4.0 * 3600.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    #[serde(default = "version")]
    pub version: u32,
    /// Set by the server; `None` for a project that is not stored.
    #[serde(default)]
    pub id: Option<i64>,
    /// User id of the owner; set by the server.
    #[serde(default)]
    pub owner: Option<i64>,
    pub name: String,
    /// The corpus clips are drawn from unless a clip names another.
    #[serde(default)]
    pub corpus: String,
    #[serde(default = "default_fps")]
    pub fps: f64,
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
    /// Burn word captions into the picture.
    #[serde(default)]
    pub captions: bool,
    /// What made this project, when a generator did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<Generator>,
    #[serde(default)]
    pub tracks: Vec<Track>,
}

/// How a project was generated and enough to run the generator again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Generator {
    pub kind: GeneratorKind,
    /// The request the generator was given (sentence: `{text, voice, options}`;
    /// song: `{midi_id, parts, options, voice}`), seed included.
    #[serde(default)]
    pub request: Value,
    #[serde(default)]
    pub seed: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GeneratorKind {
    Ytp,
    Ytpmv,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    Video,
    Audio,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub kind: TrackKind,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub solo: bool,
    /// Linear gain applied to the track's audio.
    #[serde(default = "one")]
    pub volume: f64,
    /// The part of the generator's song this track shows (YTPMV).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
    /// The clips are a readout of the generator, not editable material:
    /// the project is played by running the generator with the track's
    /// mute/solo/volume applied to its part.
    #[serde(default)]
    pub generated: bool,
    #[serde(default)]
    pub clips: Vec<Clip>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: String,
    /// Where it sits on the timeline.
    pub start: f64,
    /// How long it plays on the timeline.
    pub duration: f64,
    pub source: ClipSource,
    #[serde(default)]
    pub effects: Vec<Effect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<Caption>,
}

/// Words shown over the picture. `words` are `(text, start, end)` in *source*
/// seconds; with none, `text` is shown for the whole clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Caption {
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<(String, f64, f64)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClipSource {
    /// A span of a corpus recording. `file` is relative to the corpus directory;
    /// `clip_id`/`word`/`take` say where it came from (for the editor).
    CorpusClip {
        corpus: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clip_id: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        word: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        take: Option<u32>,
        #[serde(rename = "in")]
        in_: f64,
        out: f64,
    },
    /// An uploaded asset (id, not a path).
    Media {
        asset: String,
        #[serde(rename = "in")]
        in_: f64,
        out: f64,
    },
    /// Notes played by an emulated sound chip; note times are clip-relative.
    Synth {
        chip: String,
        #[serde(default)]
        notes: Vec<SynthNote>,
    },
    /// Something a generator makes when the project is rendered.
    Generated {
        kind: GeneratorKind,
        #[serde(default)]
        prompt: String,
        #[serde(default)]
        options: Value,
        #[serde(default)]
        seed: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        corpus: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SynthNote {
    pub start: f64,
    pub dur: f64,
    pub pitch: u8,
    #[serde(default = "default_velocity")]
    pub velocity: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Effect {
    /// Plays `factor` times faster (picture and sound, pitch follows `Pitch`).
    Speed { factor: f64 },
    /// Holds the clip `factor` times longer, keeping pitch (the "stretched word").
    Stretch { factor: f64 },
    Reverse,
    Pitch { semitones: f64 },
    Volume { db: f64 },
    /// The existing "stutter" glitch.
    Stutter,
    /// The existing "flip" glitch.
    Mirror,
    /// Any other named glitch (`shake`, `punch`, `spin`, …).
    Glitch { name: String },
    FadeIn { seconds: f64 },
    FadeOut { seconds: f64 },
    /// Hold the last picture (and play silence) this long after the clip.
    Pause { seconds: f64 },
    /// A fragment of a word: not padded to the length of the picture.
    Subword,
}

fn version() -> u32 {
    PROJECT_VERSION
}
fn one() -> f64 {
    1.0
}
fn default_fps() -> f64 {
    25.0
}
fn default_width() -> u32 {
    480
}
fn default_height() -> u32 {
    270
}
fn default_velocity() -> u8 {
    100
}

/// A short id: `[A-Za-z0-9_-]`, 1–64 characters.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl Effect {
    /// The glitch this effect stands for, for the glitch-based ones.
    pub fn glitch_name(&self) -> Option<&str> {
        match self {
            Effect::Stutter => Some("stutter"),
            Effect::Mirror => Some("flip"),
            Effect::Glitch { name } => Some(name),
            _ => None,
        }
    }

    /// The effect for glitch `name` (the model keeps `stutter` and `flip` as
    /// their own effects).
    pub fn from_glitch(name: &str) -> Effect {
        match name {
            "stutter" => Effect::Stutter,
            "flip" => Effect::Mirror,
            other => Effect::Glitch { name: other.to_string() },
        }
    }

    fn check(&self) -> Result<(), String> {
        let range = |what: &str, v: f64, lo: f64, hi: f64| {
            if v.is_finite() && (lo..=hi).contains(&v) {
                Ok(())
            } else {
                Err(format!("{what} must be between {lo} and {hi}"))
            }
        };
        match self {
            Effect::Speed { factor } => range("speed", *factor, 0.1, 10.0),
            Effect::Stretch { factor } => range("stretch", *factor, 0.1, 16.0),
            Effect::Pitch { semitones } => range("pitch", *semitones, -48.0, 48.0),
            Effect::Volume { db } => range("volume", *db, -80.0, 40.0),
            Effect::FadeIn { seconds } | Effect::FadeOut { seconds } => range("fade", *seconds, 0.0, 60.0),
            Effect::Pause { seconds } => range("pause", *seconds, 0.0, 60.0),
            Effect::Glitch { name } => {
                if !name.is_empty() && name.len() <= 24 && name.bytes().all(|b| b.is_ascii_lowercase()) {
                    Ok(())
                } else {
                    Err("a glitch name is 1-24 lowercase letters".into())
                }
            }
            Effect::Reverse | Effect::Stutter | Effect::Mirror | Effect::Subword => Ok(()),
        }
    }
}

impl ClipSource {
    fn check(&self) -> Result<(), String> {
        let span = |in_: f64, out: f64| {
            if in_.is_finite() && out.is_finite() && in_ >= 0.0 && out > in_ && out <= 1.0e6 {
                Ok(())
            } else {
                Err("a source span needs 0 <= in < out".to_string())
            }
        };
        let name = |what: &str, v: &str| {
            if !v.is_empty() && v.len() <= 200 && !v.chars().any(char::is_control) {
                Ok(())
            } else {
                Err(format!("{what} must be 1-200 printable characters"))
            }
        };
        match self {
            ClipSource::CorpusClip { corpus, clip_id, file, word, in_, out, .. } => {
                name("corpus", corpus)?;
                if clip_id.is_none() && file.is_none() && word.is_none() {
                    return Err("a corpus clip needs a clip_id, a file or a word".into());
                }
                if let Some(f) = file {
                    name("file", f)?;
                }
                span(*in_, *out)
            }
            ClipSource::Media { asset, in_, out } => {
                if !valid_id(asset) {
                    return Err("a media asset id is 1-64 characters of A-Z a-z 0-9 _ -".into());
                }
                span(*in_, *out)
            }
            ClipSource::Synth { chip, notes } => {
                name("chip", chip)?;
                if notes.len() > MAX_NOTES {
                    return Err(format!("at most {MAX_NOTES} notes in one synth clip"));
                }
                for n in notes {
                    if !(n.start.is_finite() && n.dur.is_finite() && n.start >= 0.0 && n.dur > 0.0)
                        || n.pitch > 127
                        || n.velocity > 127
                    {
                        return Err("a synth note needs start >= 0, dur > 0, pitch and velocity 0-127".into());
                    }
                }
                Ok(())
            }
            ClipSource::Generated { prompt, seed, corpus, .. } => {
                if prompt.chars().count() > 4000 {
                    return Err("a generated clip's prompt is at most 4000 characters".into());
                }
                if seed.len() > 64 || seed.chars().any(char::is_control) {
                    return Err("a seed is at most 64 printable characters".into());
                }
                if let Some(c) = corpus {
                    name("corpus", c)?;
                }
                Ok(())
            }
        }
    }

    /// Does the source have a picture of its own?
    pub fn has_picture(&self) -> bool {
        !matches!(self, ClipSource::Synth { .. })
    }
}

impl Clip {
    pub fn end(&self) -> f64 {
        self.start + self.duration
    }

    pub fn has_effect(&self, f: impl Fn(&Effect) -> bool) -> bool {
        self.effects.iter().any(f)
    }

    fn check(&self) -> Result<(), String> {
        if !valid_id(&self.id) {
            return Err("a clip id is 1-64 characters of A-Z a-z 0-9 _ -".into());
        }
        if !(self.start.is_finite() && self.start >= 0.0) {
            return Err("a clip's start must be 0 or more".into());
        }
        if !(self.duration.is_finite() && self.duration > 0.0) {
            return Err("a clip's duration must be more than 0".into());
        }
        if self.end() > MAX_SECONDS {
            return Err(format!("a clip may not end after {MAX_SECONDS} s"));
        }
        self.source.check()?;
        if self.effects.len() > 64 {
            return Err("at most 64 effects on a clip".into());
        }
        for e in &self.effects {
            e.check()?;
        }
        if let Some(l) = &self.label {
            if l.chars().count() > 200 {
                return Err("a label is at most 200 characters".into());
            }
        }
        if let Some(c) = &self.caption {
            if c.text.chars().count() > 400 || c.words.len() > 512 {
                return Err("a caption is at most 400 characters and 512 words".into());
            }
            if c.words.iter().any(|(w, a, b)| w.chars().count() > 100 || !a.is_finite() || !b.is_finite()) {
                return Err("a caption word needs text and finite times".into());
            }
        }
        Ok(())
    }
}

impl Track {
    pub fn new(id: &str, kind: TrackKind, name: &str) -> Track {
        Track {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            mute: false,
            solo: false,
            volume: 1.0,
            part: None,
            generated: false,
            clips: Vec::new(),
        }
    }

    pub fn duration(&self) -> f64 {
        self.clips.iter().map(Clip::end).fold(0.0, f64::max)
    }

    /// Clips that overlap another clip of the same track, as `(earlier, later)` ids.
    pub fn overlaps(&self) -> Vec<(String, String)> {
        let mut order: Vec<&Clip> = self.clips.iter().collect();
        order.sort_by(|a, b| a.start.total_cmp(&b.start));
        let mut out = Vec::new();
        for w in order.windows(2) {
            // Frame-rounding slack: converted projects are laid end to end.
            if w[1].start < w[0].end() - 1e-6 {
                out.push((w[0].id.clone(), w[1].id.clone()));
            }
        }
        out
    }
}

impl Project {
    pub fn new(name: &str) -> Project {
        Project {
            version: PROJECT_VERSION,
            id: None,
            owner: None,
            name: name.to_string(),
            corpus: String::new(),
            fps: 25.0,
            width: 480,
            height: 270,
            captions: false,
            generator: None,
            tracks: Vec::new(),
        }
    }

    /// End of the last clip, seconds.
    pub fn duration(&self) -> f64 {
        self.tracks.iter().map(Track::duration).fold(0.0, f64::max)
    }

    pub fn track(&self, id: &str) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn clip_count(&self) -> usize {
        self.tracks.iter().map(|t| t.clips.len()).sum()
    }

    /// Is the track heard? Mute silences it; with any solo, only soloed tracks sound.
    pub fn audible(&self, track: &Track) -> bool {
        !track.mute && (!self.tracks.iter().any(|t| t.solo) || track.solo)
    }

    /// Check the whole document; the first problem found, in words a person
    /// can act on.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != PROJECT_VERSION {
            return Err(format!(
                "project format version {} is not supported (this build reads {PROJECT_VERSION})",
                self.version
            ));
        }
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > 120 || name.chars().any(char::is_control) {
            return Err("a project name is 1-120 printable characters".into());
        }
        if self.corpus.len() > 200 {
            return Err("the corpus name is too long".into());
        }
        if !(self.fps.is_finite() && self.fps >= 1.0 && self.fps <= 120.0) {
            return Err("fps must be between 1 and 120".into());
        }
        if !(16..=7680).contains(&self.width) || !(16..=4320).contains(&self.height) {
            return Err("width and height must be between 16 and 7680 x 4320".into());
        }
        if self.tracks.len() > MAX_TRACKS {
            return Err(format!("at most {MAX_TRACKS} tracks"));
        }
        if self.clip_count() > MAX_CLIPS {
            return Err(format!("at most {MAX_CLIPS} clips"));
        }
        let mut track_ids = HashSet::new();
        let mut clip_ids = HashSet::new();
        for t in &self.tracks {
            let at = |m: String| format!("track {}: {m}", t.id);
            if !valid_id(&t.id) {
                return Err(at("a track id is 1-64 characters of A-Z a-z 0-9 _ -".into()));
            }
            if !track_ids.insert(t.id.as_str()) {
                return Err(at("the id is used twice".into()));
            }
            if t.name.chars().count() > 120 || t.name.chars().any(char::is_control) {
                return Err(at("the name is at most 120 printable characters".into()));
            }
            if !(t.volume.is_finite() && (0.0..=4.0).contains(&t.volume)) {
                return Err(at("volume must be between 0 and 4".into()));
            }
            for c in &t.clips {
                c.check().map_err(|m| at(format!("clip {}: {m}", c.id)))?;
                if !clip_ids.insert(c.id.as_str()) {
                    return Err(at(format!("clip id {} is used twice", c.id)));
                }
                if t.kind == TrackKind::Audio && matches!(c.source, ClipSource::Synth { .. }) {
                    continue;
                }
                if t.kind == TrackKind::Video && matches!(c.source, ClipSource::Synth { .. }) {
                    return Err(at(format!("clip {}: a synth clip has no picture; use an audio track", c.id)));
                }
            }
        }
        if let Some(g) = &self.generator {
            if g.seed.len() > 64 {
                return Err("the generator seed is too long".into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus_clip(id: &str, start: f64, dur: f64) -> Clip {
        Clip {
            id: id.into(),
            start,
            duration: dur,
            source: ClipSource::CorpusClip {
                corpus: "c".into(),
                clip_id: Some(1),
                file: Some("v.mp4".into()),
                word: Some("hi".into()),
                take: None,
                in_: 1.0,
                out: 1.5,
            },
            effects: vec![],
            label: None,
            caption: None,
        }
    }

    fn project() -> Project {
        let mut p = Project::new("p");
        let mut v = Track::new("v1", TrackKind::Video, "Video");
        v.clips.push(corpus_clip("a", 0.0, 0.5));
        v.clips.push(corpus_clip("b", 0.5, 0.5));
        p.tracks.push(v);
        p
    }

    #[test]
    fn json_roundtrip_and_defaults() {
        let json = r#"{"name":"p","tracks":[{"id":"v","kind":"video","clips":[
            {"id":"c","start":1.0,"duration":0.5,
             "source":{"type":"corpus_clip","corpus":"c","clip_id":1,"in":0.0,"out":0.5}}]}]}"#;
        let p: Project = serde_json::from_str(json).unwrap();
        assert_eq!(p.version, PROJECT_VERSION);
        assert_eq!((p.fps, p.width, p.height), (25.0, 480, 270));
        assert_eq!(p.tracks[0].volume, 1.0);
        assert!((p.duration() - 1.5).abs() < 1e-9);
        p.validate().unwrap();
        let back = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Project>(&back).unwrap(), p);
    }

    #[test]
    fn every_source_and_effect_roundtrips() {
        let mut p = project();
        let mut a = Track::new("a1", TrackKind::Audio, "Chip");
        a.clips.push(Clip {
            id: "s".into(),
            start: 0.0,
            duration: 1.0,
            source: ClipSource::Synth {
                chip: "nes-pulse".into(),
                notes: vec![SynthNote { start: 0.0, dur: 0.5, pitch: 60, velocity: 100 }],
            },
            effects: vec![Effect::Volume { db: -3.0 }, Effect::Pitch { semitones: 2.0 }],
            label: Some("lead".into()),
            caption: None,
        });
        a.clips.push(Clip {
            id: "m".into(),
            start: 1.0,
            duration: 1.0,
            source: ClipSource::Media { asset: "abc123".into(), in_: 0.0, out: 1.0 },
            effects: vec![Effect::Reverse, Effect::Stutter, Effect::Mirror, Effect::Glitch { name: "shake".into() }],
            label: None,
            caption: Some(Caption { text: "hi".into(), words: vec![("hi".into(), 0.0, 0.2)] }),
        });
        a.clips.push(Clip {
            id: "g".into(),
            start: 2.0,
            duration: 2.0,
            source: ClipSource::Generated {
                kind: GeneratorKind::Ytp,
                prompt: "hello world".into(),
                options: serde_json::json!({"speed": 1.1}),
                seed: "7".into(),
                corpus: None,
            },
            effects: vec![
                Effect::Speed { factor: 1.5 },
                Effect::Stretch { factor: 2.0 },
                Effect::FadeIn { seconds: 0.1 },
                Effect::FadeOut { seconds: 0.1 },
                Effect::Pause { seconds: 0.03 },
                Effect::Subword,
            ],
            label: None,
            caption: None,
        });
        p.tracks.push(a);
        p.generator = Some(Generator { kind: GeneratorKind::Ytp, request: serde_json::json!({"text": "x"}), seed: "7".into() });
        p.validate().unwrap();
        let text = serde_json::to_string_pretty(&p).unwrap();
        assert!(text.contains("\"type\": \"corpus_clip\""));
        assert!(text.contains("\"in\": 1.0"));
        assert_eq!(serde_json::from_str::<Project>(&text).unwrap(), p);
    }

    #[test]
    fn validation_catches_the_usual_mistakes() {
        let bad = |f: &dyn Fn(&mut Project)| {
            let mut p = project();
            f(&mut p);
            p.validate().expect_err("should be refused")
        };
        assert!(bad(&|p| p.version = 9).contains("version"));
        assert!(bad(&|p| p.name = "  ".into()).contains("name"));
        assert!(bad(&|p| p.fps = 0.0).contains("fps"));
        assert!(bad(&|p| p.width = 3).contains("width"));
        assert!(bad(&|p| p.tracks[0].id = "bad id".into()).contains("track id"));
        assert!(bad(&|p| p.tracks[0].volume = 9.0).contains("volume"));
        assert!(bad(&|p| p.tracks[0].clips[1].id = "a".into()).contains("twice"));
        assert!(bad(&|p| p.tracks[0].clips[0].start = -1.0).contains("start"));
        assert!(bad(&|p| p.tracks[0].clips[0].duration = 0.0).contains("duration"));
        assert!(bad(&|p| p.tracks[0].clips[0].duration = f64::NAN).contains("duration"));
        assert!(bad(&|p| p.tracks[0].clips[0].effects.push(Effect::Speed { factor: 0.0 })).contains("speed"));
        assert!(bad(&|p| p.tracks[0].clips[0].effects.push(Effect::Glitch { name: "No Way".into() })).contains("glitch"));
        assert!(bad(&|p| {
            p.tracks[0].clips[0].source = ClipSource::CorpusClip {
                corpus: "c".into(),
                clip_id: Some(1),
                file: None,
                word: None,
                take: None,
                in_: 2.0,
                out: 1.0,
            }
        })
        .contains("0 <= in < out"));
        assert!(bad(&|p| {
            p.tracks[0].clips[0].source = ClipSource::CorpusClip {
                corpus: "c".into(),
                clip_id: None,
                file: None,
                word: None,
                take: None,
                in_: 0.0,
                out: 1.0,
            }
        })
        .contains("clip_id"));
        assert!(bad(&|p| {
            p.tracks[0].clips[0].source = ClipSource::Synth { chip: "nes".into(), notes: vec![] }
        })
        .contains("audio track"));
        assert!(bad(&|p| p.tracks[0].clips[0].source = ClipSource::Media { asset: "../x".into(), in_: 0.0, out: 1.0 })
            .contains("asset"));
    }

    #[test]
    fn overlaps_duration_and_solo() {
        let mut p = project();
        assert!(p.tracks[0].overlaps().is_empty());
        p.tracks[0].clips[1].start = 0.25;
        assert_eq!(p.tracks[0].overlaps(), vec![("a".to_string(), "b".to_string())]);
        assert!((p.duration() - 0.75).abs() < 1e-9);

        let mut q = project();
        q.tracks.push(Track::new("v2", TrackKind::Video, "B"));
        assert!(q.audible(&q.tracks[0]) && q.audible(&q.tracks[1]));
        q.tracks[1].solo = true;
        assert!(!q.audible(&q.tracks[0]) && q.audible(&q.tracks[1]));
        q.tracks[1].mute = true;
        assert!(!q.audible(&q.tracks[1]));
    }

    #[test]
    fn glitch_aliases() {
        assert_eq!(Effect::from_glitch("flip"), Effect::Mirror);
        assert_eq!(Effect::from_glitch("stutter"), Effect::Stutter);
        assert_eq!(Effect::from_glitch("shake"), Effect::Glitch { name: "shake".into() });
        assert_eq!(Effect::Mirror.glitch_name(), Some("flip"));
        assert_eq!(Effect::Reverse.glitch_name(), None);
    }
}
