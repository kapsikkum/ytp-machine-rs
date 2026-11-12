//! Read a MIDI file and say, in plain words, what the song is doing
//! (port of the note/part extraction in `app/ytpmv/music.py`, on `midly`).
//!
//! A YTPMV needs to know what each part *is* before it can pick a word for
//! it: General MIDI says most of that outright (program numbers, channel 10
//! for drums); the rest — which part is the tune, what key it is in — is read
//! from the notes themselves. Nothing here touches audio.
//!
//! Not ported: the repair pass for tracks whose status bytes were
//! channel-overflowed by a buggy writer (`_repair`/`_unoverflow`), and mido's
//! clamping of data bytes above 127 (midly masks them instead). A track that
//! midly refuses is dropped and the rest of the file is still read, as
//! `_salvage` does.

use std::collections::HashMap;
use std::fmt;

use midly::{MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};

use crate::pitch::note_name;

pub const DRUM_CHANNEL: u8 = 9;

pub const GM_PROGRAMS: [&str; 128] = [
    "Acoustic Grand Piano", "Bright Acoustic Piano", "Electric Grand Piano", "Honky-tonk Piano",
    "Electric Piano 1", "Electric Piano 2", "Harpsichord", "Clavinet",
    "Celesta", "Glockenspiel", "Music Box", "Vibraphone",
    "Marimba", "Xylophone", "Tubular Bells", "Dulcimer",
    "Drawbar Organ", "Percussive Organ", "Rock Organ", "Church Organ",
    "Reed Organ", "Accordion", "Harmonica", "Tango Accordion",
    "Nylon Guitar", "Steel Guitar", "Jazz Guitar", "Clean Guitar",
    "Muted Guitar", "Overdriven Guitar", "Distortion Guitar", "Guitar Harmonics",
    "Acoustic Bass", "Fingered Bass", "Picked Bass", "Fretless Bass",
    "Slap Bass 1", "Slap Bass 2", "Synth Bass 1", "Synth Bass 2",
    "Violin", "Viola", "Cello", "Contrabass",
    "Tremolo Strings", "Pizzicato Strings", "Orchestral Harp", "Timpani",
    "String Ensemble 1", "String Ensemble 2", "Synth Strings 1", "Synth Strings 2",
    "Choir Aahs", "Voice Oohs", "Synth Voice", "Orchestra Hit",
    "Trumpet", "Trombone", "Tuba", "Muted Trumpet",
    "French Horn", "Brass Section", "Synth Brass 1", "Synth Brass 2",
    "Soprano Sax", "Alto Sax", "Tenor Sax", "Baritone Sax",
    "Oboe", "English Horn", "Bassoon", "Clarinet",
    "Piccolo", "Flute", "Recorder", "Pan Flute",
    "Blown Bottle", "Shakuhachi", "Whistle", "Ocarina",
    "Square Lead", "Saw Lead", "Calliope Lead", "Chiff Lead",
    "Charang Lead", "Voice Lead", "Fifths Lead", "Bass + Lead",
    "New Age Pad", "Warm Pad", "Polysynth Pad", "Choir Pad",
    "Bowed Pad", "Metallic Pad", "Halo Pad", "Sweep Pad",
    "Rain FX", "Soundtrack FX", "Crystal FX", "Atmosphere FX",
    "Brightness FX", "Goblins FX", "Echoes FX", "Sci-fi FX",
    "Sitar", "Banjo", "Shamisen", "Koto",
    "Kalimba", "Bagpipe", "Fiddle", "Shanai",
    "Tinkle Bell", "Agogo", "Steel Drums", "Woodblock",
    "Taiko Drum", "Melodic Tom", "Synth Drum", "Reverse Cymbal",
    "Guitar Fret Noise", "Breath Noise", "Seashore", "Bird Tweet",
    "Telephone Ring", "Helicopter", "Applause", "Gunshot",
];

const FAMILY: [&str; 16] = [
    "piano", "chromatic percussion", "organ", "guitar", "bass", "strings", "ensemble", "brass", "reed", "pipe",
    "synth lead", "synth pad", "synth effects", "ethnic", "percussive", "sound effects",
];

const DRUM_ORDER: [&str; 6] = ["kick", "snare", "hats", "toms", "cymbals", "perc"];

const MAJOR: [f64; 12] = [6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88];
const MINOR: [f64; 12] = [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17];
const PC: [&str; 12] = ["C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B"];

/// General MIDI's drum kit, folded into the handful of sounds a grid can show.
pub fn drum_group(note: u8) -> &'static str {
    match note {
        35 | 36 => "kick",
        31 | 37 | 38 | 39 | 40 => "snare",
        42 | 44 | 46 | 54 | 69 | 70 | 82 => "hats",
        41 | 43 | 45 | 47 | 48 | 50 | 60..=68 => "toms",
        49 | 51 | 52 | 53 | 55 | 57 | 59 => "cymbals",
        _ => "perc",
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    /// seconds
    pub start: f64,
    pub dur: f64,
    /// MIDI note number
    pub pitch: u8,
    pub velocity: u8,
}

#[derive(Clone, Debug)]
pub struct Part {
    pub id: String,
    pub name: String,
    pub track: usize,
    pub channel: u8,
    /// `None` for drums
    pub program: Option<u8>,
    pub notes: Vec<Note>,
    /// bass | lead | chords | rhythm | drums:<group>
    pub role: String,
    pub drum_group: Option<String>,
    pub pan: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct PartSummary {
    pub id: String,
    pub name: String,
    pub role: String,
    pub instrument: String,
    pub is_drums: bool,
    pub notes: usize,
    pub range: Option<(String, String)>,
    pub range_midi: (u8, u8),
    pub median_midi: f64,
    pub common_midi: u8,
    pub pan: Option<f64>,
    pub polyphony: usize,
    pub typical_polyphony: f64,
    pub first: Option<f64>,
}

impl Part {
    pub fn is_drums(&self) -> bool {
        self.drum_group.is_some()
    }

    pub fn instrument(&self) -> String {
        match self.drum_group.as_deref() {
            Some("kick") => "Kick drum".into(),
            Some("snare") => "Snare".into(),
            Some("hats") => "Hi-hats".into(),
            Some("toms") => "Toms".into(),
            Some("cymbals") => "Cymbals".into(),
            Some(_) => "Percussion".into(),
            None => GM_PROGRAMS[self.program.unwrap_or(0) as usize].to_string(),
        }
    }

    /// The pitch the part sits on, weighted by how long each note lasts.
    pub fn median_pitch(&self) -> f64 {
        if self.notes.is_empty() {
            return 60.0;
        }
        let mut ws: Vec<(u8, f64)> = self.notes.iter().map(|n| (n.pitch, n.dur)).collect();
        ws.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)));
        let half = ws.iter().map(|w| w.1).sum::<f64>() / 2.0;
        let mut acc = 0.0;
        for (p, d) in &ws {
            acc += d;
            if acc >= half {
                return *p as f64;
            }
        }
        ws[ws.len() - 1].0 as f64
    }

    /// The most common pitch (first seen wins ties).
    pub fn common_pitch(&self) -> u8 {
        let mut order: Vec<u8> = Vec::new();
        let mut counts: HashMap<u8, usize> = HashMap::new();
        for n in &self.notes {
            if !counts.contains_key(&n.pitch) {
                order.push(n.pitch);
            }
            *counts.entry(n.pitch).or_insert(0) += 1;
        }
        let mut best: Option<u8> = None;
        for p in order {
            if best.is_none_or(|b| counts[&p] > counts[&b]) {
                best = Some(p);
            }
        }
        best.unwrap_or(60)
    }

    /// (most notes ever sounding at once, typical number while any sound).
    pub fn polyphony(&self) -> (usize, f64) {
        let mut ev: Vec<(f64, i32)> = Vec::with_capacity(self.notes.len() * 2);
        for n in &self.notes {
            ev.push((n.start, 1));
        }
        for n in &self.notes {
            ev.push((n.start + n.dur, -1));
        }
        ev.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal).then(a.1.cmp(&b.1)));
        let (mut cur, mut mx) = (0i32, 0i32);
        let (mut weighted, mut active) = (0.0f64, 0.0f64);
        let mut last: Option<f64> = None;
        for (t, d) in ev {
            if let Some(l) = last {
                if cur > 0 {
                    weighted += cur as f64 * (t - l);
                    active += t - l;
                }
            }
            cur += d;
            mx = mx.max(cur);
            last = Some(t);
        }
        (mx.max(0) as usize, if active > 0.0 { weighted / active } else { 0.0 })
    }

    pub fn summary(&self) -> PartSummary {
        let (mx, typ) = self.polyphony();
        let lo = self.notes.iter().map(|n| n.pitch).min().unwrap_or(0);
        let hi = self.notes.iter().map(|n| n.pitch).max().unwrap_or(0);
        PartSummary {
            id: self.id.clone(),
            name: self.name.clone(),
            role: self.role.clone(),
            instrument: self.instrument(),
            is_drums: self.is_drums(),
            notes: self.notes.len(),
            range: if self.is_drums() { None } else { Some((note_name(lo as f64), note_name(hi as f64))) },
            range_midi: (lo, hi),
            median_midi: self.median_pitch(),
            common_midi: self.common_pitch(),
            pan: self.pan.map(|p| (p * 100.0).round_ties_even() / 100.0),
            polyphony: mx,
            typical_polyphony: (typ * 100.0).round_ties_even() / 100.0,
            first: self.notes.first().map(|n| (n.start * 1000.0).round_ties_even() / 1000.0),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Song {
    pub parts: Vec<Part>,
    pub duration: f64,
    pub bpm: f64,
    pub time_signature: (u32, u32),
    pub key: String,
    pub key_confidence: f64,
    pub title: String,
    pub tempo_changes: usize,
}

impl Song {
    pub fn part(&self, pid: &str) -> Option<&Part> {
        self.parts.iter().find(|p| p.id == pid)
    }

    pub fn describe(&self) -> String {
        let bpm = self.bpm;
        let pace = if bpm < 76.0 {
            "slow"
        } else if bpm < 100.0 {
            "relaxed"
        } else if bpm < 128.0 {
            "upbeat"
        } else if bpm < 160.0 {
            "fast"
        } else {
            "very fast"
        };
        let mood = if self.key.ends_with("minor") { "dark" } else { "bright" };
        let melodic: Vec<&Part> = self.parts.iter().filter(|p| !p.is_drums()).collect();
        let mut fams: Vec<(&str, usize)> = Vec::new();
        for p in &melodic {
            let fam = FAMILY[(p.program.unwrap_or(0) / 8) as usize];
            match fams.iter_mut().find(|(f, _)| *f == fam) {
                Some((_, n)) => *n += 1,
                None => fams.push((fam, 1)),
            }
        }
        fams.sort_by_key(|a| std::cmp::Reverse(a.1));
        let heavy = melodic.iter().any(|p| matches!(p.program, Some(29) | Some(30)));
        let mut bits: Vec<String> = fams
            .iter()
            .map(|(fam, n)| {
                if *n == 1 {
                    fam.to_string()
                } else {
                    format!("{n} {fam}{}", if fam.ends_with('s') { "" } else { "s" })
                }
            })
            .collect();
        let drums = self.parts.iter().filter(|p| p.is_drums()).count();
        if drums > 0 {
            bits.push(format!("{drums} drum sound{}", if drums > 1 { "s" } else { "" }));
        }
        let secs = self.duration.round_ties_even() as i64;
        let (m, s) = (secs.div_euclid(60), secs.rem_euclid(60));
        let mut pace_cap = pace.to_string();
        pace_cap[..1].make_ascii_uppercase();
        format!(
            "{pace_cap} ({bpm:.0} BPM), {}, {mood}{}, {m}:{s:02} long: {}.",
            self.key,
            if heavy { " and heavy" } else { "" },
            bits.join(", ")
        )
    }
}

#[derive(Debug)]
pub enum MidiError {
    NotMidi,
    NoReadableTrack,
    Parse(String),
}

impl fmt::Display for MidiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MidiError::NotMidi => write!(f, "not a MIDI file"),
            MidiError::NoReadableTrack => write!(f, "no track in the file could be read"),
            MidiError::Parse(e) => write!(f, "MIDI parse error: {e}"),
        }
    }
}

impl std::error::Error for MidiError {}

// ── Tempo map ────────────────────────────────────────────────────────────────

struct TempoMap {
    tpb: i64,
    ticks: Vec<i64>,
    tempi: Vec<i64>,
    secs: Vec<f64>,
}

impl TempoMap {
    fn new(tpb: i64, changes: &[(i64, i64)]) -> Self {
        let tpb = if tpb.abs() == 0 { 480 } else { tpb.abs() };
        let mut ch = changes.to_vec();
        ch.sort();
        if ch.is_empty() || ch[0].0 != 0 {
            ch.insert(0, (0, 500000));
        }
        let ticks: Vec<i64> = ch.iter().map(|c| c.0).collect();
        let tempi: Vec<i64> = ch.iter().map(|c| if c.1 > 0 { c.1 } else { 500000 }).collect();
        let mut secs = vec![0.0f64];
        for i in 1..ch.len() {
            let dt = (ticks[i] - ticks[i - 1]) as f64;
            let prev = secs[secs.len() - 1];
            secs.push(prev + dt * tempi[i - 1] as f64 / 1e6 / tpb as f64);
        }
        TempoMap { tpb, ticks, tempi, secs }
    }

    fn seconds(&self, tick: i64) -> f64 {
        let tick = tick.max(0);
        let i = self.ticks.partition_point(|t| *t <= tick).saturating_sub(1);
        self.secs[i] + (tick - self.ticks[i]) as f64 * self.tempi[i] as f64 / 1e6 / self.tpb as f64
    }

    /// The tempo the song spends longest in.
    fn main_bpm(&self, end_tick: i64) -> f64 {
        let mut spans: Vec<(i64, i64)> = Vec::new();
        for (i, t) in self.ticks.iter().enumerate() {
            let nxt = if i + 1 < self.ticks.len() { self.ticks[i + 1] } else { end_tick.max(*t) };
            let add = (nxt - t) * self.tempi[i];
            match spans.iter_mut().find(|(tempo, _)| *tempo == self.tempi[i]) {
                Some((_, s)) => *s += add,
                None => spans.push((self.tempi[i], add)),
            }
        }
        let mut best: Option<(i64, i64)> = None;
        for (tempo, span) in spans {
            if best.is_none_or(|(_, b)| span > b) {
                best = Some((tempo, span));
            }
        }
        let tempo = best.map(|b| b.0).filter(|t| *t > 0).unwrap_or(500000);
        60e6 / tempo as f64
    }
}

// ── Key ──────────────────────────────────────────────────────────────────────

fn corr(a: &[f64], b: &[f64]) -> f64 {
    let ma = a.iter().sum::<f64>() / 12.0;
    let mb = b.iter().sum::<f64>() / 12.0;
    let num: f64 = a.iter().zip(b.iter()).map(|(x, y)| (x - ma) * (y - mb)).sum();
    let da: f64 = a.iter().map(|x| (x - ma).powi(2)).sum();
    let db: f64 = b.iter().map(|y| (y - mb).powi(2)).sum();
    let den = (da * db).powf(0.5);
    if den != 0.0 {
        num / den
    } else {
        0.0
    }
}

fn pc_index(name: &str) -> usize {
    PC.iter().position(|p| *p == name).unwrap_or(0)
}

/// The key a set of parts is in, with a 0..1 confidence.
pub fn detect_key(parts: &[Part]) -> (String, f64) {
    let mut hist = [0.0f64; 12];
    for p in parts.iter().filter(|p| !p.is_drums()) {
        for n in &p.notes {
            hist[(n.pitch % 12) as usize] += n.dur;
        }
    }
    let total: f64 = hist.iter().sum();
    if total == 0.0 {
        return ("no key".into(), 0.0);
    }

    let mut scores: Vec<(f64, String)> = Vec::new();
    for tonic in 0..12 {
        let rot: Vec<f64> = (0..12).map(|i| hist[(tonic + i) % 12]).collect();
        scores.push((corr(&rot, &MAJOR), format!("{} major", PC[tonic])));
        scores.push((corr(&rot, &MINOR), format!("{} minor", PC[tonic])));
    }
    let by_name: HashMap<String, f64> = scores.iter().map(|(s, n)| (n.clone(), *s)).collect();
    scores.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal).then_with(|| b.1.cmp(&a.1)));
    let (mut best, second) = (scores[0].clone(), scores[1].clone());
    let conf = best.0.clamp(0.0, 1.0) * (0.5 + (best.0 - second.0) * 5.0).min(1.0);

    let mut parts_split = best.1.split(' ');
    let t0 = pc_index(parts_split.next().unwrap());
    let mode0 = parts_split.next().unwrap();
    let rel = if mode0 == "major" { (t0 + 9) % 12 } else { (t0 + 3) % 12 };
    let rel_name = format!("{} {}", PC[rel], if mode0 == "major" { "minor" } else { "major" });
    if best.0 - by_name[&rel_name] < 0.15 {
        let (mut v_t0, mut v_rel) = (0.0f64, 0.0f64);
        let melodic: Vec<&Part> = parts.iter().filter(|p| !p.is_drums() && !p.notes.is_empty()).collect();
        if !melodic.is_empty() {
            let mut low = melodic[0];
            let mut high = melodic[0];
            for p in &melodic {
                if p.median_pitch() < low.median_pitch() {
                    low = p;
                }
                if p.median_pitch() > high.median_pitch() {
                    high = p;
                }
            }
            let mut bass: Vec<&Note> = low.notes.iter().collect();
            bass.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap().then(a.pitch.cmp(&b.pitch)));
            let mut tune: Vec<&Note> = high.notes.iter().collect();
            tune.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap().then(b.pitch.cmp(&a.pitch)));
            for pc in [bass[0].pitch % 12, bass[bass.len() - 1].pitch % 12, tune[0].pitch % 12] {
                if pc as usize == t0 {
                    v_t0 += 1.0;
                } else if pc as usize == rel {
                    v_rel += 1.0;
                }
            }
        }
        let hmax = hist.iter().cloned().fold(f64::MIN, f64::max);
        v_t0 += 0.1 * hist[t0] / hmax;
        v_rel += 0.1 * hist[rel] / hmax;
        if v_rel > v_t0 {
            best = (by_name[&rel_name], rel_name.clone());
        }
    }

    let tonic = pc_index(best.1.split(' ').next().unwrap());
    let minor = hist[(tonic + 3) % 12] + 0.5 * hist[(tonic + 8) % 12];
    let major = hist[(tonic + 4) % 12] + 0.5 * hist[(tonic + 9) % 12];
    if minor + major > 0.02 * total && (minor - major).abs() > 0.25 * (minor + major) {
        return (format!("{} {}", PC[tonic], if minor > major { "minor" } else { "major" }), conf);
    }
    (best.1, conf)
}

// ── Roles ────────────────────────────────────────────────────────────────────

fn assign_roles(parts: &mut [Part]) {
    struct Info {
        median: f64,
        mx: usize,
        typ: f64,
        n: usize,
    }
    let info: Vec<Option<Info>> = parts
        .iter()
        .map(|p| {
            if p.is_drums() {
                None
            } else {
                let (mx, typ) = p.polyphony();
                Some(Info { median: p.median_pitch(), mx, typ, n: p.notes.len() })
            }
        })
        .collect();
    for p in parts.iter_mut() {
        if p.is_drums() {
            p.role = format!("drums:{}", p.drum_group.as_deref().unwrap_or("perc"));
        }
    }
    let melodic: Vec<usize> = (0..parts.len()).filter(|i| !parts[*i].is_drums()).collect();
    let mut basses: Vec<usize> = melodic
        .iter()
        .copied()
        .filter(|i| matches!(parts[*i].program, Some(32..=39)))
        .collect();
    if basses.is_empty() {
        let low: Vec<usize> = melodic
            .iter()
            .copied()
            .filter(|i| {
                let inf = info[*i].as_ref().unwrap();
                inf.median < 48.0 && inf.typ < 1.5
            })
            .collect();
        if !low.is_empty() && melodic.len() > 1 {
            let mut best = low[0];
            for i in &low {
                if info[*i].as_ref().unwrap().median < info[best].as_ref().unwrap().median {
                    best = *i;
                }
            }
            basses = vec![best];
        }
    }
    let mut rest = Vec::new();
    for &i in &melodic {
        let inf = info[i].as_ref().unwrap();
        let _ = inf.mx;
        if basses.contains(&i) {
            parts[i].role = "bass".into();
        } else if inf.typ >= 2.5 {
            parts[i].role = "chords".into();
        } else {
            rest.push(i);
        }
    }
    if !rest.is_empty() {
        let key = |i: usize| {
            let inf = info[i].as_ref().unwrap();
            ((inf.median / 3.0).round_ties_even() as i64, inf.n)
        };
        let mut lead = rest[0];
        for &i in &rest {
            if key(i) > key(lead) {
                lead = i;
            }
        }
        for &i in &rest {
            parts[i].role = if i == lead { "lead".into() } else { "rhythm".into() };
        }
    }
}

// ── Reading the file ─────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
enum Ev {
    Tempo(i64),
    TimeSig(u32, u32),
    Program(u8, u8),
    Pan(u8, u8),
    NoteOn(u8, u8, u8),
    NoteOff(u8, u8),
    /// any other non-meta message; only moves the song's end tick
    Other,
    /// a meta event that matters nowhere else: time passes, nothing happens
    Meta,
}

struct TrackData {
    events: Vec<(i64, Ev)>,
    name: Option<String>,
}

fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|b| *b as char).collect()
}

fn convert_track(track: &[midly::TrackEvent]) -> TrackData {
    let mut events = Vec::with_capacity(track.len());
    let mut name = None;
    for ev in track {
        let delta = ev.delta.as_int() as i64;
        let e = match ev.kind {
            TrackEventKind::Meta(MetaMessage::Tempo(t)) => Some(Ev::Tempo(t.as_int() as i64)),
            TrackEventKind::Meta(MetaMessage::TimeSignature(n, d, _, _)) => {
                Some(Ev::TimeSig(n as u32, 1u32.checked_shl(d as u32).unwrap_or(0)))
            }
            TrackEventKind::Meta(MetaMessage::TrackName(b)) => {
                let s = latin1(b).trim().to_string();
                if !s.is_empty() {
                    name = Some(s);
                }
                None
            }
            TrackEventKind::Meta(_) => Some(Ev::Meta),
            TrackEventKind::Midi { channel, message } => {
                let ch = channel.as_int();
                Some(match message {
                    MidiMessage::ProgramChange { program } => Ev::Program(ch, program.as_int()),
                    MidiMessage::Controller { controller, value } if controller.as_int() == 10 => {
                        Ev::Pan(ch, value.as_int())
                    }
                    MidiMessage::NoteOn { key, vel } => Ev::NoteOn(ch, key.as_int(), vel.as_int()),
                    MidiMessage::NoteOff { key, .. } => Ev::NoteOff(ch, key.as_int()),
                    _ => Ev::Other,
                })
            }
            TrackEventKind::SysEx(_) | TrackEventKind::Escape(_) => Some(Ev::Other),
        };
        events.push((delta, e.unwrap_or(Ev::Meta)));
    }
    TrackData { events, name }
}

fn read_tracks(data: &[u8]) -> Result<(i64, Vec<TrackData>), MidiError> {
    if data.len() < 14 || &data[..4] != b"MThd" {
        return Err(MidiError::NotMidi);
    }
    match Smf::parse(data) {
        Ok(smf) => {
            let tpb = match smf.header.timing {
                Timing::Metrical(t) => t.as_int() as i64,
                Timing::Timecode(..) => 480,
            };
            Ok((if tpb == 0 { 480 } else { tpb }, smf.tracks.iter().map(|t| convert_track(t)).collect()))
        }
        Err(first) => salvage(data).map_err(|e| match e {
            MidiError::NoReadableTrack => MidiError::Parse(first.to_string()),
            other => other,
        }),
    }
}

/// The tracks of *data* that read, when one that does not sinks the file.
fn salvage(data: &[u8]) -> Result<(i64, Vec<TrackData>), MidiError> {
    let hlen = u32::from_be_bytes(data[4..8].try_into().unwrap()) as usize;
    let head_end = 8 + hlen;
    if data.len() < head_end || hlen < 6 {
        return Err(MidiError::NotMidi);
    }
    let division = [data[12], data[13]];
    let tpb = (u16::from_be_bytes(division) & 0x7FFF) as i64;
    let mut good = Vec::new();
    let mut i = head_end;
    while i + 8 <= data.len() {
        let n = u32::from_be_bytes(data[i + 4..i + 8].try_into().unwrap()) as usize;
        let end = (i + 8 + n).min(data.len());
        if &data[i..i + 4] == b"MTrk" {
            let mut one = b"MThd".to_vec();
            one.extend_from_slice(&6u32.to_be_bytes());
            one.extend_from_slice(&[0, 1, 0, 1]);
            one.extend_from_slice(&division);
            one.extend_from_slice(&data[i..end]);
            if let Ok(smf) = Smf::parse(&one) {
                if let Some(t) = smf.tracks.first() {
                    good.push(convert_track(t));
                }
            }
        }
        i += 8 + n;
    }
    if good.is_empty() {
        return Err(MidiError::NoReadableTrack);
    }
    Ok((if tpb == 0 { 480 } else { tpb }, good))
}

type RawNote = (i64, i64, u8, u8, bool);

/// Read a MIDI file into a [`Song`].
pub fn parse(data: &[u8], title: &str) -> Result<Song, MidiError> {
    let (tpb, tracks) = read_tracks(data)?;

    let mut tempo_changes: Vec<(i64, i64)> = Vec::new();
    let mut time_sig = (4u32, 4u32);
    let mut programs: HashMap<u8, u8> = HashMap::new();
    let mut end_tick = 0i64;
    let mut raw: std::collections::BTreeMap<(usize, u8), Vec<RawNote>> = std::collections::BTreeMap::new();
    // (track, channel) -> program counts in order of first use
    let mut chan_prog: HashMap<(usize, u8), Vec<(u8, usize)>> = HashMap::new();
    let mut names: HashMap<usize, String> = HashMap::new();
    let mut pans: HashMap<(usize, u8), f64> = HashMap::new();

    for (ti, track) in tracks.iter().enumerate() {
        let mut tick = 0i64;
        // (channel, note) -> queue of (start tick, velocity)
        let mut open: Vec<((u8, u8), std::collections::VecDeque<(i64, u8)>)> = Vec::new();
        if let Some(n) = &track.name {
            names.insert(ti, n.clone());
        }
        for (delta, ev) in &track.events {
            tick += delta;
            match *ev {
                Ev::Meta => continue,
                Ev::Tempo(t) => {
                    tempo_changes.push((tick, t));
                    continue;
                }
                Ev::TimeSig(n, d) => {
                    if tick == 0 {
                        time_sig = (n, d);
                    }
                    continue;
                }
                Ev::Program(ch, p) => {
                    programs.insert(ch, p);
                    continue;
                }
                Ev::Pan(ch, v) => {
                    pans.insert((ti, ch), ((v as f64 - 64.0) / 64.0).clamp(-1.0, 1.0));
                    continue;
                }
                Ev::NoteOn(ch, note, vel) if vel > 0 => {
                    match open.iter_mut().find(|(k, _)| *k == (ch, note)) {
                        Some((_, q)) => q.push_back((tick, vel)),
                        None => open.push(((ch, note), std::collections::VecDeque::from([(tick, vel)]))),
                    }
                    let prog = programs.get(&ch).copied().unwrap_or(0);
                    let counts = chan_prog.entry((ti, ch)).or_default();
                    match counts.iter_mut().find(|(p, _)| *p == prog) {
                        Some((_, c)) => *c += 1,
                        None => counts.push((prog, 1)),
                    }
                }
                Ev::NoteOn(ch, note, _) | Ev::NoteOff(ch, note) => {
                    if let Some((_, q)) = open.iter_mut().find(|(k, _)| *k == (ch, note)) {
                        if let Some((st, vel)) = q.pop_front() {
                            raw.entry((ti, ch)).or_default().push((st, tick, note, vel, true));
                        }
                    }
                }
                Ev::Other => {}
            }
            end_tick = end_tick.max(tick);
        }
        for ((ch, pitch), q) in open {
            for (st, vel) in q {
                raw.entry((ti, ch)).or_default().push((st, tick, pitch, vel, false));
            }
        }
    }

    let tm = TempoMap::new(tpb, &tempo_changes);
    let last = raw
        .values()
        .flat_map(|evs| evs.iter())
        .filter(|(_, _, _, _, rel)| *rel)
        .map(|(_, e, ..)| *e)
        .max()
        .map(|e| tm.seconds(e));
    let end = |s: i64, e: i64, rel: bool| -> f64 {
        let e_s = tm.seconds(e);
        match last {
            Some(l) if !rel => e_s.min(tm.seconds(s).max(l) + 4.0),
            _ => e_s,
        }
    };

    let mut parts: Vec<Part> = Vec::new();
    for ((ti, ch), mut evs) in raw {
        evs.sort_by_key(|a| (a.0, a.1, a.2, a.3, a.4));
        let notes: Vec<Note> = evs
            .iter()
            .map(|&(s, e, pch, vel, rel)| {
                let st = tm.seconds(s);
                Note { start: st, dur: (end(s, e, rel) - st).max(0.0), pitch: pch, velocity: vel }
            })
            .collect();
        let pan = pans.get(&(ti, ch)).copied();
        if ch == DRUM_CHANNEL {
            let mut groups: Vec<(&str, Vec<Note>)> = Vec::new();
            for n in notes {
                let g = drum_group(n.pitch);
                match groups.iter_mut().find(|(k, _)| *k == g) {
                    Some((_, v)) => v.push(n),
                    None => groups.push((g, vec![n])),
                }
            }
            for g in DRUM_ORDER {
                if let Some((_, v)) = groups.iter().find(|(k, _)| *k == g) {
                    if !v.is_empty() {
                        parts.push(Part {
                            id: format!("t{ti}c{ch}-{g}"),
                            name: g.to_string(),
                            track: ti,
                            channel: ch,
                            program: None,
                            notes: v.clone(),
                            role: "rhythm".into(),
                            drum_group: Some(g.to_string()),
                            pan,
                        });
                    }
                }
            }
        } else {
            let prog = chan_prog
                .get(&(ti, ch))
                .and_then(|counts| {
                    let mut best: Option<(u8, usize)> = None;
                    for &(p, c) in counts {
                        if best.is_none_or(|(_, bc)| c > bc) {
                            best = Some((p, c));
                        }
                    }
                    best.map(|b| b.0)
                })
                .unwrap_or(0);
            parts.push(Part {
                id: format!("t{ti}c{ch}"),
                name: names.get(&ti).cloned().unwrap_or_else(|| GM_PROGRAMS[prog as usize].to_string()),
                track: ti,
                channel: ch,
                program: Some(prog),
                notes,
                role: "rhythm".into(),
                drum_group: None,
                pan,
            });
        }
    }

    assign_roles(&mut parts);

    // friendlier, unique names: "Distortion Guitar (lead)"
    let mut seen: HashMap<String, usize> = HashMap::new();
    for p in parts.iter_mut().filter(|p| !p.is_drums()) {
        let base = format!("{} ({})", p.instrument(), p.role);
        let c = seen.entry(base.clone()).or_insert(0);
        *c += 1;
        p.name = if *c == 1 { base } else { format!("{base} {c}") };
    }

    // melodic parts first (lead, chords, rhythm, bass), then the kit in kit order
    let order = |p: &Part| -> (u8, usize) {
        if let Some(g) = &p.drum_group {
            (1, DRUM_ORDER.iter().position(|d| d == g).unwrap_or(0))
        } else {
            (
                0,
                match p.role.as_str() {
                    "lead" => 0,
                    "chords" => 1,
                    "rhythm" => 2,
                    "bass" => 3,
                    _ => 9,
                },
            )
        }
    };
    parts.sort_by_key(order);

    let (key, conf) = detect_key(&parts);
    let duration = parts
        .iter()
        .flat_map(|p| p.notes.iter())
        .map(|n| n.start + n.dur)
        .fold(0.0f64, f64::max);
    let mut uniq = tempo_changes.clone();
    uniq.sort();
    uniq.dedup();
    Ok(Song {
        parts,
        duration,
        bpm: tm.main_bpm(end_tick),
        time_signature: time_sig,
        key,
        key_confidence: conf,
        title: title.to_string(),
        tempo_changes: uniq.len().saturating_sub(1),
    })
}

/// Octaves to move a part so it sits near the voice singing it.
pub fn auto_octave(median_midi: f64, sample_midi: Option<f64>) -> i32 {
    match sample_midi {
        None => 0,
        Some(s) => ((s - median_midi) / 12.0).round_ties_even().clamp(-3.0, 3.0) as i32,
    }
}
