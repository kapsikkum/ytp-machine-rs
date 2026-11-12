//! Clips (corpus rows with their neighbours) and segments (what the encoder
//! is asked to play). Segments are the Python app's segment dicts: every key
//! that dict could carry is a field here.

use serde_json::{json, Map, Value};

/// How a take sounds: standardised features and its tone category.
#[derive(Debug, Clone, PartialEq)]
pub struct Tone {
    /// In `FEATURES` order: pitch, pitch_start, pitch_end, movement,
    /// loudness, pace, brightness.
    pub z: [Option<f64>; 7],
    pub cat: Option<i64>,
}

pub const F_PITCH_START: usize = 1;
pub const F_PITCH_END: usize = 2;
pub const F_LOUDNESS: usize = 4;
pub const F_BRIGHTNESS: usize = 6;

/// A recording of one word in a source video.
#[derive(Debug, Clone)]
pub struct Clip {
    pub id: Option<i64>,
    pub source_id: i64,
    pub word: String,
    pub start_time: f64,
    pub end_time: f64,
    /// Absolute path.
    pub source_file: String,
    /// End of the clip before it in the same source.
    pub prev_end: Option<f64>,
    /// Start of the clip after it.
    pub next_start: Option<f64>,
    pub bleed_risk: bool,
    pub edited: bool,
    pub tone: Option<Tone>,
    /// Stored aligned phonemes: `(phone, start, end)` from `start_time`.
    pub phones: Option<Vec<(String, f64, f64)>>,
}

impl Clip {
    pub fn duration(&self) -> f64 {
        self.end_time - self.start_time
    }
}

/// Which sounds a splice unit was cut for.
#[derive(Debug, Clone, PartialEq)]
pub struct Unit {
    pub at: usize,
    pub phones: Vec<String>,
    pub from: String,
    pub clip_id: Option<i64>,
}

/// One piece of the video: a span of a source, with how to play it.
#[derive(Debug, Clone, Default)]
pub struct Segment {
    pub id: Option<i64>,
    pub source_id: i64,
    pub word: String,
    pub start_time: f64,
    pub end_time: f64,
    pub source_file: String,
    pub prev_end: Option<f64>,
    pub next_start: Option<f64>,
    pub bleed_risk: bool,
    pub edited: bool,
    pub tone: Option<Tone>,
    pub phones: Option<Vec<(String, f64, f64)>>,

    /// `None` = the default fade.
    pub fade_in: Option<f64>,
    pub fade_out: Option<f64>,
    pub pause_after: Option<f64>,
    pub stretch: Option<f64>,
    pub subword: bool,
    pub reverse: bool,
    pub pitch: Option<f64>,
    pub gain_db: Option<f64>,
    pub glitch: Vec<String>,
    pub subtitle: Option<String>,
    pub subtitle_words: Vec<(String, f64, f64)>,
    /// Index of the token this segment says.
    pub tok: Option<usize>,
    /// A run: `(token index, source start, source end)` per word.
    pub tokens: Vec<(usize, f64, f64)>,
    pub unit: Option<Unit>,
    pub spliced_from: Option<String>,
    pub matched: Option<usize>,
    pub approx: bool,
    /// A stretch of on-screen silence used for a full stop.
    pub idle: bool,
    pub src_start: Option<f64>,
    pub src_end: Option<f64>,
    pub cut: bool,
}

impl Segment {
    pub fn from_clip(c: &Clip) -> Self {
        Self {
            id: c.id,
            source_id: c.source_id,
            word: c.word.clone(),
            start_time: c.start_time,
            end_time: c.end_time,
            source_file: c.source_file.clone(),
            prev_end: c.prev_end,
            next_start: c.next_start,
            bleed_risk: c.bleed_risk,
            edited: c.edited,
            tone: c.tone.clone(),
            phones: c.phones.clone(),
            ..Default::default()
        }
    }

    pub fn pause(&self) -> f64 {
        self.pause_after.unwrap_or(0.0)
    }

    pub fn is_spoken(&self) -> bool {
        self.tok.is_some() || !self.tokens.is_empty()
    }
}

/// How one token was satisfied.
#[derive(Debug, Clone, Default)]
pub struct TokenReport {
    pub word: String,
    /// `found`, `run`, `spliced` or `missing`.
    pub status: String,
    pub clips: Option<Vec<i64>>,
    pub approx: bool,
    pub stretched: Option<String>,
    pub voice: Option<String>,
    pub shown: Option<String>,
}

impl TokenReport {
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("word".into(), json!(self.word));
        m.insert("status".into(), json!(self.status));
        if let Some(c) = &self.clips {
            m.insert("clips".into(), json!(c));
        }
        if self.approx {
            m.insert("approx".into(), json!(true));
        }
        if let Some(s) = &self.stretched {
            m.insert("stretched".into(), json!(s));
        }
        if let Some(v) = &self.voice {
            m.insert("voice".into(), json!(v));
        }
        if let Some(s) = &self.shown {
            m.insert("shown".into(), json!(s));
        }
        Value::Object(m)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub found: Vec<String>,
    pub spliced: Vec<String>,
    pub missing: Vec<String>,
    pub runs: Vec<String>,
    pub tokens: Vec<TokenReport>,
}

impl Report {
    pub fn to_json(&self) -> Value {
        json!({
            "found": self.found, "spliced": self.spliced, "missing": self.missing,
            "runs": self.runs,
            "tokens": self.tokens.iter().map(TokenReport::to_json).collect::<Vec<_>>(),
        })
    }
}
