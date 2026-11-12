//! Per-part and per-song settings: what a request may change, held to range,
//! and how a whole-song chip switch is pushed through the parts
//! (`_default_settings_for`, `_merge`, `_through_chip` and the option parsing
//! of `app/ytpmv/render.py`).

use std::collections::HashMap;

use serde_json::{json, Map, Value};
use ytp_music::channels::overrides_switch;
use ytp_music::music::{Part, Song};
use ytp_music::routing::{bass_line, chip_tone, floor_hz, octaves_to_fit, which_chip};
use ytp_music::tone::{resolve, synth_tones};

use crate::layout::GRID_LAYOUTS;
use crate::recommend::Rec;
use crate::screen;

/// How a part avoids sounding like one recording on a loop.
pub const VARY: &[(&str, &str)] = &[
    ("off", "the one take, every time"),
    ("rotate", "the best few takes in turn"),
    ("random", "those takes in no order"),
    ("ultra", "a different word every hit, each sung on the note"),
];
pub const MODES: [&str; 3] = ["perfect", "tape", "raw"];
/// Words a part throws about in the ultra mode.
pub const ULTRA_POOL: usize = 5;

/// The machines a song can be played on, and what to call them. Each has two
/// ways of going about it, because each machine had two: a synthesiser, and a
/// channel for playing back a recording.
pub const CHIPS: &[(&str, &str)] = &[
    ("md", "the Mega Drive's YM2612, synthesising: four-operator FM"),
    ("md-voice", "the voice, played off the Mega Drive's 8-bit sample channel"),
    ("nes", "the NES's 2A03, synthesising: two pulses, a triangle and noise"),
    ("nes-voice", "the voice, played off the NES's delta-modulation channel"),
    ("sms", "the Master System's SN76489: three squares and a shift register"),
    ("sms-voice", "the voice, hammered out of the Master System's volume register"),
    ("snes", "the SNES's S-DSP playing a General MIDI bank cut down to fit it"),
    ("snes-voice", "the voice through the SNES's sampler, grain and echo and all"),
    ("gb", "the Game Boy's chip: two pulses, a wave table and noise; green screen"),
    ("gb-voice", "the voice through a Game Boy's wave table; green screen"),
    ("gbc", "the same chip as the Game Boy, on the Color's screen"),
    ("gbc-voice", "the voice through the same wave table, on the Color's screen"),
    ("opl2", "an AdLib's OPL2 playing Doom's instrument bank; EGA screen"),
    ("opl2-voice", "the voice off an original Sound Blaster; EGA screen"),
    ("opl3", "a Sound Blaster's OPL3, same bank, eight waveforms; VGA screen"),
    ("opl3-voice", "the voice off a sixteen-bit Sound Blaster; VGA screen"),
    ("sid", "the C64's 6581 SID, filter and all; C64 screen"),
    ("sid-voice", "the voice hammered out of the 6581's volume register; C64 screen"),
    ("sid8580", "the later 8580 SID, cleaner filter; C64 screen"),
    ("sid8580-voice", "the same trick on an 8580, where it barely works; C64 screen"),
];

/// Which console's picture goes with `chip`: the same machine's, or none.
pub fn screen_for(chip: Option<&str>) -> &'static str {
    let Some(chip) = chip else { return "none" };
    let machine = chip.split('-').next().unwrap_or("");
    let machine = match machine {
        "opl2" => "ega",
        "opl3" => "vga",
        "sid" | "sid8580" => "c64",
        m => m,
    };
    screen::SIZES.iter().map(|(n, _, _)| *n).find(|n| *n == machine).unwrap_or("none")
}

fn volume_of(role: &str) -> f64 {
    match role {
        "lead" | "bass" => 1.0,
        "rhythm" => 0.75,
        "chords" => 0.7,
        // The kick sits a little above the rest: it is the only part whose
        // job is to be felt.
        "drums:kick" => 1.2,
        "drums:snare" => 0.9,
        "drums:hats" => 0.55,
        "drums:toms" => 0.8,
        "drums:cymbals" => 0.5,
        "drums:perc" => 0.7,
        _ => 0.8,
    }
}

/// How loud a role sits unless told otherwise (also the reference for a trim).
pub fn default_volume(role: &str) -> f64 {
    volume_of(role)
}

fn pan_of(role: &str) -> f64 {
    match role {
        "rhythm" => -0.35,
        "chords" => 0.35,
        "drums:snare" => 0.05,
        "drums:hats" => 0.3,
        "drums:toms" => -0.2,
        "drums:cymbals" => -0.3,
        "drums:perc" => 0.25,
        _ => 0.0,
    }
}

// ── Reading JSON the way the Python app's `_num` does ────────────────────────

pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(*b as u8 as f64),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn as_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f.trunc() as i64)),
        Value::Bool(b) => Some(*b as i64),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// `_num(v, default, lo, hi)` for floats.
pub fn num_f(v: Option<&Value>, default: f64, lo: f64, hi: f64) -> f64 {
    match v.and_then(as_f64) {
        Some(x) if x.is_finite() => x.clamp(lo, hi),
        _ => default,
    }
}

/// `_num(v, default, lo, hi, int)`.
pub fn num_i(v: Option<&Value>, default: i64, lo: i64, hi: i64) -> i64 {
    match v.and_then(as_i64) {
        Some(x) => x.clamp(lo, hi),
        None => default,
    }
}

/// What a request's `chip` option means: `true` is the Mega Drive.
pub fn chip_option(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::Bool(true)) => Some("md".into()),
        Some(Value::String(s)) => which_chip(s),
        _ => None,
    }
}

// ── Part settings ────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct PartSettings {
    pub id: String,
    pub corpus: String,
    pub text: Option<String>,
    pub take: usize,
    pub takes: Vec<usize>,
    pub pool: Vec<(String, usize)>,
    pub octave: i32,
    pub transpose: i32,
    pub volume: f64,
    pub pan: f64,
    pub haas: bool,
    pub mode: String,
    pub sustain: String,
    pub tone: String,
    pub program: Option<u8>,
    pub hit: bool,
    pub mute: bool,
    pub visible: bool,
    pub chip: Option<String>,
}

impl PartSettings {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id, "corpus": self.corpus, "text": self.text, "take": self.take, "takes": self.takes,
            "pool": self.pool.iter().map(|(t, k)| json!({ "text": t, "take": k })).collect::<Vec<_>>(),
            "octave": self.octave, "transpose": self.transpose, "volume": self.volume, "pan": self.pan,
            "haas": self.haas, "mode": self.mode, "sustain": self.sustain, "tone": self.tone,
            "program": self.program, "hit": self.hit, "mute": self.mute, "visible": self.visible,
            "chip": self.chip,
        })
    }
}

pub fn default_settings_for(part: &Part, rec: &Rec, corpus: &str) -> PartSettings {
    let take = rec.take;
    let text = rec.text.clone();
    PartSettings {
        id: part.id.clone(),
        corpus: corpus.to_string(),
        text: text.clone(),
        take,
        // The best few takes of this word, for the variety modes to rotate
        // through when one is turned on, and the other words that would have
        // done, for the ultra mode to throw about.
        takes: if rec.takes.is_empty() { vec![take] } else { rec.takes.clone() },
        pool: if rec.pool.is_empty() { vec![(text.clone().unwrap_or_default(), take)] } else { rec.pool.clone() },
        octave: rec.octave,
        transpose: 0,
        volume: volume_of(&part.role),
        pan: part.pan.unwrap_or_else(|| pan_of(&part.role)),
        haas: false,
        mode: if rec.mode.is_empty() { (if part.is_drums() { "raw" } else { "perfect" }).to_string() } else { rec.mode.clone() },
        sustain: if part.is_drums() { "ring" } else { "note" }.to_string(),
        // What the part is played through, and always nothing until asked.
        tone: "clean".to_string(),
        // The General MIDI instrument, which is what picks an OPL patch and
        // nudges which voice the other chips choose.
        program: part.program,
        // Drums are cut down to a hit (the "b" of a word, not the word).
        hit: part.is_drums(),
        mute: text.is_none(),
        visible: text.is_some(),
        chip: None,
    }
}

/// `defaults` with the request's choices applied, each held to its range.
pub fn merge(defaults: &PartSettings, given: Option<&Value>) -> PartSettings {
    let mut s = defaults.clone();
    let Some(given) = given.and_then(Value::as_object) else { return s };
    if let Some(c) = given.get("corpus").and_then(Value::as_str) {
        s.corpus = c.to_string();
    }
    let given_text = given.get("text").and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty());
    if let Some(t) = given_text {
        s.text = Some(t.chars().take(60).collect());
        if t.to_lowercase() != defaults.text.clone().unwrap_or_default() && !given.contains_key("take") {
            s.take = 0;
        }
    }
    s.take = num_i(given.get("take").or(Some(&json!(s.take))), s.take as i64, 0, 10_000) as usize;
    if let Some(list) = given.get("takes").and_then(Value::as_array) {
        let picked: Vec<usize> = list.iter().take(6).map(|t| num_i(Some(t), 0, 0, 10_000) as usize).collect();
        s.takes = if picked.is_empty() { vec![s.take] } else { picked };
    } else if given.contains_key("take") || given_text.is_some() {
        // A word or take chosen by hand is the one meant; nothing rotates away
        // from it, and the ultra mode throws that one word about instead.
        s.takes = vec![s.take];
        s.pool = vec![(s.text.clone().unwrap_or_default(), s.take)];
    }
    if let Some(list) = given.get("pool").and_then(Value::as_array) {
        let chosen: Vec<(String, usize)> = list
            .iter()
            .take(ULTRA_POOL)
            .filter_map(|e| {
                let e = e.as_object()?;
                let t = e.get("text").and_then(Value::as_str).filter(|t| !t.is_empty())?;
                Some((t.chars().take(60).collect(), num_i(e.get("take"), 0, 0, 10_000) as usize))
            })
            .collect();
        if !chosen.is_empty() {
            s.pool = chosen;
        }
    }
    s.octave = num_i(given.get("octave"), s.octave as i64, -4, 4) as i32;
    s.transpose = num_i(given.get("transpose"), 0, -24, 24) as i32;
    s.volume = num_f(given.get("volume"), s.volume, 0.0, 2.0);
    s.pan = num_f(given.get("pan"), s.pan, -1.0, 1.0);
    if let Some(m) = given.get("mode").and_then(Value::as_str).filter(|m| MODES.contains(m)) {
        s.mode = m.to_string();
    }
    if let Some(m) = given.get("sustain").and_then(Value::as_str).filter(|m| *m == "note" || *m == "ring") {
        s.sustain = m.to_string();
    }
    if let Some(t) = given.get("tone").and_then(Value::as_str).and_then(resolve) {
        s.tone = t;
    }
    if let (Some(dp), Some(p)) = (defaults.program, given.get("program").filter(|p| !p.is_null())) {
        s.program = Some(num_i(Some(p), dp as i64, 0, 127) as u8);
    }
    if given.contains_key("hit") {
        s.hit = truthy(given.get("hit"));
    }
    if given.contains_key("haas") {
        s.haas = truthy(given.get("haas"));
    }
    if s.text.is_some() && defaults.text.is_none() {
        // Given a sound where none was suggested: it is meant to be heard.
        s.mute = false;
        s.visible = true;
    }
    if given.contains_key("mute") {
        s.mute = truthy(given.get("mute"));
    }
    if given.contains_key("visible") {
        s.visible = truthy(given.get("visible"));
    }
    s
}

/// Put the parts in `settings` through `chip`, in place.
///
/// A part that was given a tone of its own keeps it; every other part gets
/// whatever the chip plays that part with. With `synth` there is no voice:
/// only a synthesised tone can be kept, every part plays whether or not a
/// word was found for it, and its label is what it is played by.
pub fn through_chip(
    song: &Song,
    settings: &mut HashMap<String, PartSettings>,
    given: &HashMap<String, Value>,
    chip: Option<&str>,
    g_transpose: i32,
    synth: bool,
) {
    let null = Value::Null;
    let low = if chip.is_some() { bass_line(song) } else { None };
    for p in &song.parts {
        let Some(s) = settings.get_mut(&p.id) else { continue };
        let asked = given.get(&p.id).unwrap_or(&null);
        let asked_tone = asked.get("tone").and_then(Value::as_str);
        // Every part knows the chip, chosen tone or not: a SID voice asked for
        // by name on a song through the 8580 is played by the 8580.
        s.chip = chip.map(str::to_string);
        let mut keep = overrides_switch(asked_tone);
        if synth {
            keep = keep && asked_tone.and_then(resolve).is_some_and(|t| synth_tones().contains(&t));
        }
        if let (Some(chip), false) = (chip, keep) {
            s.tone = chip_tone(p, chip, low.as_deref() == Some(p.id.as_str()), s.program, !synth);
        }
        if synth || synth_tones().contains(&s.tone) {
            // Muted for want of a word is not muted: there are no words.
            s.text = Some(s.tone.clone());
            s.mute = truthy(asked.get("mute"));
            s.visible = asked.get("visible").is_none_or(|v| truthy(Some(v)));
        }
        // The octave a part was moved by is there because a mouth cannot go
        // where the score does. A chip has no such limit, and those shifts
        // are not musical decisions — so a part a chip is playing goes back
        // to the octave it was written in, unless one was asked for.
        if asked.get("octave").is_none() && synth_tones().contains(&s.tone) {
            s.octave = 0;
        }
    }

    // Some machines cannot go as low as a part is written: the Master System
    // stops at 109 Hz, an NES pulse at 55. Such a part moves up by whole
    // octaves, all of it together, until its lowest note is playable. Unless
    // an octave was asked for, in which case the asker has heard it.
    for p in &song.parts {
        let Some(s) = settings.get_mut(&p.id) else { continue };
        let floor = floor_hz(&s.tone);
        let asked = given.get(&p.id).unwrap_or(&null);
        if floor <= 0.0 || p.is_drums() || asked.get("octave").is_some() {
            continue;
        }
        let shift = 12 * s.octave + s.transpose + g_transpose;
        s.octave += octaves_to_fit(&p.notes, floor, shift);
    }
}

/// The song-wide options of a render request.
#[derive(Clone, Debug)]
pub struct Options {
    pub max_seconds: f64,
    pub speed: f64,
    pub g_transpose: i32,
    pub flip: bool,
    pub flash: bool,
    pub dim: bool,
    pub labels: bool,
    pub vary: String,
    pub jitter: bool,
    /// Measure every part and set it where its job says it should sit.
    pub balance: bool,
    /// Only as many notes at once as the chip had channels.
    pub accurate: bool,
    pub chip: Option<String>,
    /// No voice: the corpus is left out, every part is the chip.
    pub synth: bool,
    pub layout: String,
    pub cover: bool,
    pub center_tiles: bool,
    pub draft: bool,
    pub tile_order: Option<Vec<String>>,
}

impl Options {
    pub fn from_json(opts: &Value, cap: f64) -> Options {
        let get = |k: &str| opts.get(k);
        let flag = |k: &str, default: bool| get(k).map_or(default, |v| truthy(Some(v)));
        let vary = match get("vary") {
            Some(Value::Bool(b)) => (if *b { "rotate" } else { "off" }).to_string(),
            Some(Value::String(s)) if VARY.iter().any(|(k, _)| k == s) => s.clone(),
            _ => "off".to_string(),
        };
        let synth = flag("synth", false);
        let mut chip = chip_option(get("chip"));
        if synth {
            chip = Some(ytp_music::routing::synth_chip(chip.as_deref()));
        }
        let layout = get("layout").and_then(Value::as_str).filter(|l| GRID_LAYOUTS.contains(l)).unwrap_or("balanced");
        Options {
            max_seconds: num_f(get("max_seconds"), cap, 1.0, cap),
            speed: num_f(get("speed"), 1.0, 0.25, 4.0),
            g_transpose: num_i(get("transpose"), 0, -24, 24) as i32,
            flip: flag("flip", true),
            flash: flag("flash", true),
            dim: flag("dim", true),
            labels: flag("labels", false),
            vary,
            jitter: flag("jitter", false),
            balance: flag("balance", true),
            accurate: flag("accurate", false),
            chip,
            synth,
            layout: layout.to_string(),
            cover: get("media_fit").and_then(Value::as_str) == Some("cover"),
            center_tiles: flag("center_tiles", true),
            draft: flag("draft", false),
            tile_order: get("tile_order")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()),
        }
    }

    pub fn screen(&self) -> &'static str {
        screen_for(self.chip.as_deref())
    }

    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("vary".into(), json!(self.vary));
        m.insert("jitter".into(), json!(self.jitter));
        m.insert("speed".into(), json!(self.speed));
        m.insert("balance".into(), json!(self.balance));
        m.insert("accurate".into(), json!(self.accurate));
        m.insert("screen".into(), json!(self.screen()));
        m.insert("synth".into(), json!(self.synth));
        m.insert("chip".into(), json!(self.chip));
        m.insert("transpose".into(), json!(self.g_transpose));
        m.insert("max_seconds".into(), json!(self.max_seconds));
        m.insert("layout".into(), json!(self.layout));
        m.insert("media_fit".into(), json!(if self.cover { "cover" } else { "contain" }));
        m.insert("center_tiles".into(), json!(self.center_tiles));
        Value::Object(m)
    }
}
