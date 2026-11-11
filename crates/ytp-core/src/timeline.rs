//! Timeline model. Placeholder for the studio/splicing phases: the shape is
//! here so later crates agree on it, the semantics are not final.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: Option<i64>,
    pub name: String,
    /// Corpus the clips are drawn from.
    pub corpus: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    Video,
    Audio,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub name: String,
    pub kind: TrackKind,
    #[serde(default)]
    pub muted: bool,
    pub clips: Vec<Clip>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    /// `word_clips.id` in the corpus database.
    pub clip_id: i64,
    pub word: String,
    /// Position on the timeline, seconds.
    pub at: f64,
    /// Trim inside the source clip, seconds relative to the clip start.
    pub trim_start: f64,
    pub trim_end: f64,
    #[serde(default = "one")]
    pub speed: f64,
    #[serde(default)]
    pub pitch_semitones: f64,
    #[serde(default)]
    pub reverse: bool,
    #[serde(default = "one")]
    pub gain: f64,
}

fn one() -> f64 {
    1.0
}

impl Project {
    pub fn duration(&self) -> f64 {
        self.tracks
            .iter()
            .flat_map(|t| &t.clips)
            .map(|c| c.at + (c.trim_end - c.trim_start).max(0.0) / c.speed.max(f64::EPSILON))
            .fold(0.0, f64::max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_duration() {
        let json = r#"{"id":null,"name":"p","corpus":"c","width":1280,"height":720,"fps":30,
            "tracks":[{"name":"v","kind":"video","clips":[
                {"clip_id":1,"word":"hi","at":1.0,"trim_start":0.0,"trim_end":0.5}]}]}"#;
        let p: Project = serde_json::from_str(json).unwrap();
        assert_eq!(p.tracks[0].clips[0].speed, 1.0);
        assert!((p.duration() - 1.5).abs() < 1e-9);
        let back = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Project>(&back).unwrap(), p);
    }
}
