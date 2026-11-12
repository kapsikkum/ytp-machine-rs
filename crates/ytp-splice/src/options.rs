//! What a person generating a sentence may change, each value held to its
//! range (`OPTIONS` / `generation_options` in `app/generate.py`).

use serde_json::{json, Value};

pub const WORD_GAP: f64 = 0.03;
pub const STOP_PAUSE: f64 = 0.50;

#[derive(Debug, Clone, PartialEq)]
pub struct GenerationOptions {
    pub speed: f64,
    pub pitch: f64,
    pub word_gap: f64,
    pub sentence_pause: f64,
    pub stretch: f64,
    pub clean_takes: bool,
    pub phrases: bool,
    pub tone_match: f64,
    pub sing: bool,
    pub sing_key: f64,
    pub sing_shape: f64,
    pub sing_amount: f64,
    pub sing_vibrato: f64,
    pub sing_cute: f64,
    pub sing_midi: String,
    pub sing_part: String,
    pub vocode: bool,
    pub pause_style: f64,
    pub chaos: f64,
    pub seed: String,
}

impl Default for GenerationOptions {
    fn default() -> Self {
        Self::from_json(&Value::Null)
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn as_float(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(*b as u8 as f64),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

impl GenerationOptions {
    /// `generation_options(given)`: defaults filled in, values pulled into range.
    pub fn from_json(given: &Value) -> Self {
        let num = |key: &str, default: f64, lo: f64, hi: f64| -> f64 {
            let v = given
                .get(key)
                .map_or(Some(default), as_float)
                .unwrap_or(default);
            let v = if v.is_nan() { default } else { v };
            v.max(lo).min(hi)
        };
        let flag = |key: &str, default: bool| -> bool {
            given.get(key).map_or(default, truthy)
        };
        let text = |key: &str| -> String {
            let raw = match given.get(key) {
                None | Some(Value::Null) => String::new(),
                Some(Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
            };
            raw.chars()
                .filter(|&c| ytp_text::is_word_char(c) || c == '.' || c == '-')
                .take(64)
                .collect()
        };
        Self {
            speed: num("speed", 1.0, 0.5, 2.0),
            pitch: num("pitch", 0.0, -12.0, 12.0),
            word_gap: num("word_gap", WORD_GAP, 0.0, 0.6),
            sentence_pause: num("sentence_pause", STOP_PAUSE, 0.0, 2.0),
            stretch: num("stretch", 0.09, 0.02, 0.35),
            clean_takes: flag("clean_takes", true),
            phrases: flag("phrases", true),
            tone_match: num("tone_match", 0.6, 0.0, 1.0),
            sing: flag("sing", false),
            sing_key: num("sing_key", 0.0, 0.0, 11.0),
            sing_shape: num("sing_shape", 0.0, 0.0, 4.0),
            sing_amount: num("sing_amount", 1.0, 0.0, 1.0),
            sing_vibrato: num("sing_vibrato", 0.0, 0.0, 1.0),
            sing_cute: num("sing_cute", 0.0, 0.0, 1.0),
            sing_midi: text("sing_midi"),
            sing_part: text("sing_part"),
            vocode: flag("vocode", false),
            pause_style: num("pause_style", 0.0, 0.0, 2.0),
            chaos: num("chaos", 0.0, 0.0, 1.0),
            seed: text("seed"),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "speed": self.speed, "pitch": self.pitch, "word_gap": self.word_gap,
            "sentence_pause": self.sentence_pause, "stretch": self.stretch,
            "clean_takes": self.clean_takes, "phrases": self.phrases,
            "tone_match": self.tone_match, "sing": self.sing, "sing_key": self.sing_key,
            "sing_shape": self.sing_shape, "sing_amount": self.sing_amount,
            "sing_vibrato": self.sing_vibrato, "sing_cute": self.sing_cute,
            "sing_midi": self.sing_midi, "sing_part": self.sing_part,
            "vocode": self.vocode, "pause_style": self.pause_style, "chaos": self.chaos,
            "seed": self.seed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_and_defaults() {
        let o = GenerationOptions::from_json(&json!({
            "speed": 9, "pitch": "x", "word_gap": -1, "phrases": 0, "seed": "a b/c.d", "chaos": 0.5
        }));
        assert_eq!(o.speed, 2.0);
        assert_eq!(o.pitch, 0.0);
        assert_eq!(o.word_gap, 0.0);
        assert!(!o.phrases);
        assert!(o.clean_takes);
        assert_eq!(o.seed, "abc.d");
        assert_eq!(o.chaos, 0.5);
    }
}
