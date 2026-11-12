//! A song described for the page: key, tempo, parts, and what each part could
//! be played by (`Song.summary`, `render.analyse`, `render.timeline_notes`,
//! `tone.catalogue`).

use std::collections::HashSet;

use serde_json::{json, Map, Value};
use ytp_music::channels::SAMPLED;
use ytp_music::music::{Part, PartSummary, Song, GM_PROGRAMS};
use ytp_music::routing::{bass_line, chip_tone};
use ytp_music::tone::{synth_tones, tones};
use ytp_music::{gb, nes, sid, sms, ym2612};

use crate::recommend::Rec;
use crate::settings::{default_settings_for, CHIPS};
use crate::{msg, round_to, Result};

fn about(tone: &str) -> String {
    tones().iter().find(|(n, _)| n == tone).map(|(_, a)| a.clone()).unwrap_or_default()
}

pub fn part_summary_json(s: &PartSummary) -> Value {
    json!({
        "id": s.id, "name": s.name, "role": s.role, "instrument": s.instrument, "is_drums": s.is_drums,
        "notes": s.notes,
        "range": s.range.as_ref().map(|(a, b)| json!([a, b])),
        "range_midi": [s.range_midi.0, s.range_midi.1],
        "median_midi": s.median_midi, "common_midi": s.common_midi,
        "pan": s.pan, "polyphony": s.polyphony, "typical_polyphony": s.typical_polyphony, "first": s.first,
    })
}

/// The song without its parts.
pub fn summary_without_parts(song: &Song) -> Value {
    json!({
        "title": song.title,
        "duration": round_to(song.duration, 2),
        "bpm": round_to(song.bpm, 1),
        "tempo_changes": song.tempo_changes,
        "time_signature": format!("{}/{}", song.time_signature.0, song.time_signature.1),
        "key": song.key,
        "key_confidence": round_to(song.key_confidence, 2),
        "description": song.describe(),
    })
}

pub fn summary(song: &Song) -> Value {
    let mut v = summary_without_parts(song);
    v["parts"] = Value::Array(song.parts.iter().map(|p| part_summary_json(&p.summary())).collect());
    v
}

/// What the library list shows about each MIDI file.
pub fn library_info(song: &Song) -> Value {
    let drums = song.parts.iter().filter(|p| p.is_drums()).count();
    json!({
        "title": song.title,
        "duration": round_to(song.duration, 1),
        "bpm": song.bpm.round_ties_even(),
        "key": song.key,
        "time_signature": format!("{}/{}", song.time_signature.0, song.time_signature.1),
        "tempo_changes": song.tempo_changes,
        "parts": song.parts.len() - drums,
        "drums": drums,
        "notes": song.parts.iter().map(|p| p.notes.len()).sum::<usize>(),
        "description": song.describe(),
    })
}

fn pairs(names: impl Iterator<Item = String>) -> Vec<Value> {
    names.map(|t| json!([t, about(&t)])).collect()
}

/// What a part can be played by, grouped for choosing from.
pub fn catalogue() -> Value {
    let mut machines = Map::new();
    let kit = |voices: Vec<String>, drums: &[(&str, &str)]| -> Value {
        let kit: HashSet<&str> = drums.iter().map(|(_, v)| *v).collect();
        json!({
            "melodic": pairs(voices.iter().filter(|v| !kit.contains(v.as_str())).cloned()),
            "drums": pairs(voices.iter().filter(|v| kit.contains(v.as_str())).cloned()),
        })
    };
    let mut put = |key: &str, name: &str, mut body: Value| {
        body["name"] = json!(name);
        machines.insert(key.to_string(), body);
    };
    put("md", "Mega Drive", kit(ym2612::patches().iter().map(|p| p.name.to_string()).collect(), ym2612::DRUMS));
    put("nes", "NES", kit(nes::VOICES.iter().map(|p| p.name.to_string()).collect(), nes::DRUMS));
    put("sms", "Master System", kit(sms::VOICES.iter().map(|p| p.name.to_string()).collect(), sms::DRUMS));
    // One setting; the instrument picks the sound.
    let snes = pairs(["snes".to_string()].into_iter());
    put("snes", "SNES", json!({ "melodic": snes, "drums": snes }));
    put("gb", "Game Boy", kit(gb::VOICES.iter().map(|p| p.name.to_string()).collect(), gb::DRUMS));
    let opl = pairs(["opl2".to_string(), "opl3".to_string()].into_iter());
    put("opl", "OPL2 / OPL3", json!({ "melodic": opl, "drums": opl }));
    put("sid", "SID", kit(sid::VOICES.iter().map(|p| p.name.to_string()).collect(), sid::DRUMS));

    let machine_of = json!({
        "md": "md", "nes": "nes", "sms": "sms", "snes": "snes", "gb": "gb", "gbc": "gb",
        "opl2": "opl", "opl3": "opl", "sid": "sid", "sid8580": "sid",
    });
    let mut sampled: Vec<String> = Vec::new();
    for (_, t) in SAMPLED {
        if !sampled.iter().any(|s| s == t) {
            sampled.push(t.to_string());
        }
    }
    let mut voice = vec![json!(["voice", "the voice, as recorded, whatever the chip"])];
    voice.extend(pairs(sampled.into_iter()));
    let mut synth: Vec<&String> = synth_tones().iter().collect();
    synth.sort();
    json!({ "machines": machines, "machine_of": machine_of, "voice": voice, "synth": synth })
}

/// The chip settings a song can be played through, with a description each.
pub fn chips_json() -> Value {
    Value::Object(CHIPS.iter().map(|(k, v)| (k.to_string(), json!(v))).collect())
}

/// The song in plain words, plus a suggested sound for every part.
pub fn analyse(
    song: &Song,
    midi_id: &str,
    recs: &[(String, Rec)],
    corpus: &str,
    noises: &[(String, usize)],
    units: &[(String, usize)],
) -> Value {
    let mut out = summary(song);
    out["midi_id"] = json!(midi_id);
    // What this voice has besides words. A spew or a click is already a
    // percussive sound, so the page offers them for any part.
    out["noises"] = Value::Object(noises.iter().map(|(k, n)| (k.clone(), json!(n))).collect());
    out["units"] = Value::Object(units.iter().map(|(k, n)| (k.clone(), json!(n))).collect());
    out["voice"] = json!(corpus);
    out["corpus"] = json!(corpus);
    let mut instruments = catalogue();
    instruments["programs"] = json!(GM_PROGRAMS.as_slice());
    instruments["chips"] = chips_json();
    out["instruments"] = instruments;

    let low = bass_line(song);
    let machines: Vec<&str> = CHIPS.iter().map(|(k, _)| *k).collect();
    let mut tables = Map::new();
    let mut parts = Vec::new();
    for p in &song.parts {
        let rec = recs.iter().find(|(id, _)| *id == p.id).map(|(_, r)| r.clone()).unwrap_or_else(|| Rec::empty(p));
        let is_low = low.as_deref() == Some(p.id.as_str());
        let mut v = part_summary_json(&p.summary());
        v["recommended"] = rec.to_json();
        v["settings"] = default_settings_for(p, &rec, corpus).to_json();
        v["chip_tones"] = Value::Object(machines.iter().map(|c| (c.to_string(), json!(chip_tone(p, c, is_low, None, true)))).collect());
        // And with no voice, where a drum has nothing to play but the kit.
        v["chip_tones_synth"] =
            Value::Object(machines.iter().map(|c| (c.to_string(), json!(chip_tone(p, c, is_low, None, false)))).collect());
        // A bounded overview of real MIDI notes for the track workbench.
        let stride = p.notes.len().div_ceil(1024).max(1);
        v["activity"] = Value::Array(
            p.notes.iter().step_by(stride).map(|n| json!([round_to(n.start, 4), round_to(n.dur, 4), n.pitch])).collect(),
        );
        // The tone every General MIDI program gets on each chip: one table
        // per kind of part, shared.
        if !p.is_drums() {
            let key = format!("{}{}", p.role, if is_low { "-low" } else { "" });
            tables.entry(key.clone()).or_insert_with(|| program_table(p, is_low));
            v["program_table"] = json!(key);
        }
        parts.push(v);
    }
    out["parts"] = Value::Array(parts);
    out["program_tones"] = Value::Object(tables);
    out
}

fn program_table(p: &Part, is_low: bool) -> Value {
    Value::Object(
        CHIPS
            .iter()
            .filter(|(c, _)| !c.ends_with("-voice"))
            .map(|(c, _)| {
                let tones: Vec<String> = (0..GM_PROGRAMS.len()).map(|prog| chip_tone(p, c, is_low, Some(prog as u8), true)).collect();
                (c.to_string(), json!(tones))
            })
            .collect(),
    )
}

/// Read-only viewport detail: the notes of the visible parts in a range.
pub fn timeline_notes(song: &Song, start: f64, end: Option<f64>, part_ids: &[String]) -> Result<Value> {
    let end = end.unwrap_or(song.duration);
    if !start.is_finite() || !end.is_finite() || start < 0.0 || end <= start {
        return msg("Choose a finite timeline range with its end after its start.");
    }
    if part_ids.is_empty() || part_ids.len() > 64 || part_ids.iter().any(|id| song.part(id).is_none()) {
        return msg("Choose between one and 64 installed MIDI parts.");
    }
    let limit = (24000 / part_ids.len()).max(1);
    let mut parts = Map::new();
    let mut counts = Map::new();
    let mut sampled = false;
    for id in part_ids {
        if parts.contains_key(id) {
            continue;
        }
        let p = song.part(id).unwrap();
        let notes: Vec<_> = p.notes.iter().filter(|n| n.start < end && n.start + n.dur >= start).collect();
        let stride = notes.len().div_ceil(limit).max(1);
        let picked: Vec<Value> = notes.iter().step_by(stride).map(|n| json!([round_to(n.start, 5), round_to(n.dur, 5), n.pitch])).collect();
        sampled |= notes.len() > picked.len();
        counts.insert(id.clone(), json!(notes.len()));
        parts.insert(id.clone(), Value::Array(picked));
    }
    Ok(json!({ "parts": parts, "counts": counts, "sampled": sampled }))
}
