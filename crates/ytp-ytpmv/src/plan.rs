//! A song's render plan as a timeline [`Project`]: one track per part, one
//! clip per note, without synthesising or drawing anything.
//!
//! The tracks are a readout of the generator (`Track::generated`): playing the
//! project runs the song renderer again with each track's mute, solo and
//! volume applied to its part, so the render is the one the plan came from.

use ytp_core::{Clip, ClipSource, Effect, Generator, GeneratorKind, Project, SynthNote, Track, TrackKind};

use crate::mix::{plan_song, Prepared};
use crate::{round_to, FPS};

/// A track id from a part id: ids are `[A-Za-z0-9_-]` in the model.
pub fn track_id(part: &str) -> String {
    let id: String = part
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .take(56)
        .collect();
    if id.is_empty() {
        "part".into()
    } else {
        id
    }
}

/// Loudness of a note struck at `velocity`, relative to full, in dB.
fn velocity_db(velocity: u8) -> f64 {
    20.0 * (0.35 + 0.65 * velocity as f64 / 127.0).log10()
}

/// `request` is what the render would be asked (`{midi_id, parts, options,
/// voice}`); it goes into the project so the song can be rendered again.
pub fn plan_project(prep: &Prepared, name: &str, request: serde_json::Value) -> Result<Project, String> {
    let mut project = Project::new(name);
    project.width = prep.out_size.0 as u32;
    project.height = prep.out_size.1 as u32;
    project.fps = FPS as f64;
    project.generator = Some(Generator { kind: GeneratorKind::Ytpmv, request, seed: String::new() });
    let shown: std::collections::HashSet<&str> = prep.tiles.iter().map(|t| t.id.as_str()).collect();
    let total: usize = prep.song.parts.iter().map(|p| p.notes.len()).sum();
    if total > ytp_core::timeline::MAX_CLIPS {
        return Err(format!("this song has {total} notes; a project holds at most {}", ytp_core::timeline::MAX_CLIPS));
    }
    for (part_id, notes) in plan_song(prep) {
        let Some(part) = prep.song.part(&part_id) else { continue };
        let s = &prep.settings[&part_id];
        let chip = prep.chip_parts.contains(&part_id);
        let samples = prep.sample_of.get(&part_id);
        let id = track_id(&part_id);
        let kind = if !chip && shown.contains(part_id.as_str()) { TrackKind::Video } else { TrackKind::Audio };
        let mut track = Track::new(&id, kind, &part.name);
        track.mute = s.mute || !prep.playing.contains(&part_id);
        track.volume = s.volume.clamp(0.0, 4.0);
        track.part = Some(part_id.clone());
        track.generated = true;
        for (k, n) in notes.iter().enumerate() {
            let cid = format!("{id}-{k}");
            let mut effects = Vec::new();
            let source = if chip {
                ClipSource::Synth {
                    chip: s.tone.clone(),
                    notes: vec![SynthNote { start: 0.0, dur: round_to(n.dur, 5), pitch: n.midi.clamp(0, 127) as u8, velocity: n.velocity.min(127) }],
                }
            } else {
                let sample = samples.and_then(|v| v.get(n.take % v.len().max(1)));
                let (offset, len, native) = sample
                    .map(|smp| (smp.offset, smp.duration(), smp.voice.info.midi))
                    .unwrap_or((0.0, n.dur, None));
                if let (Some(native), false) = (native, part.is_drums()) {
                    let semis = n.midi as f64 - native;
                    if semis.abs() > 1e-6 {
                        effects.push(Effect::Pitch { semitones: round_to(semis, 3).clamp(-48.0, 48.0) });
                    }
                }
                ClipSource::CorpusClip {
                    corpus: s.corpus.clone(),
                    clip_id: None,
                    file: None,
                    word: s.text.clone(),
                    take: Some(n.take as u32),
                    in_: round_to(offset, 4),
                    out: round_to((offset + len).max(offset + 0.001), 4),
                }
            };
            let db = velocity_db(n.velocity);
            if db.abs() > 0.05 {
                effects.push(Effect::Volume { db: round_to(db, 2) });
            }
            if n.flip {
                effects.push(Effect::Mirror);
            }
            track.clips.push(Clip {
                id: cid,
                start: round_to(n.start, 5),
                duration: round_to(n.dur, 5).max(0.001),
                source,
                effects,
                label: None,
                caption: None,
            });
        }
        project.tracks.push(track);
    }
    project.validate()?;
    Ok(project)
}
