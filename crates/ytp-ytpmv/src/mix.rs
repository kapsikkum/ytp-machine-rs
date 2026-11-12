//! Play a MIDI file with the corpus: every note sung, every part a tile — the
//! sound half (`render._render_ytpmv`, `_play`, `preview_part`).
//!
//! The sound is built in-process: each note is the part's sample put on that
//! note by `ytp_music::pitch::Voice` (or synthesised by an emulated chip),
//! summed into one stereo mix, balanced part against part by measured
//! loudness, and mastered. Parts are played in parallel, each into a buffer
//! of its own.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use ytp_music::channels::allocate;
use ytp_music::master;
use ytp_music::music::{Note, Part, Song};
use ytp_music::pitch::{midi_to_hz, Mode, Warp};
use ytp_music::routing::chip_tone;
use ytp_music::sid::Model;
use ytp_music::tone::{self, resolve, synth_tones, ToneParams};
use ytp_music::wav;
use ytp_splice::PyRandom;

use crate::layout::{grid_cells, ordered_ids, Cell};
use crate::recommend::{recommend, AnalysisStore, Rec};
use crate::samples::{Banks, Env, Sample};
use crate::screen;
use crate::settings::{default_settings_for, default_volume, merge, num_f, through_chip, truthy, Options, PartSettings};
use crate::{msg, round_to, say, Error, Progress, Result, FPS, MAX_SECONDS, OUT_H, OUT_W, SR, SRC_W, TAIL};

/// How long a synthesised note left to ring may go on for.
const RING_MAX: f64 = 1.5;
const ULTRA_POOL: usize = crate::settings::ULTRA_POOL;
const HAAS_DELAY: f64 = 0.018;

/// One hit of a part: when the note lands, how long it is heard, and how the
/// picture follows (the sample's time-warp).
#[derive(Clone, Debug)]
pub struct Hit {
    pub start: f64,
    pub dur: f64,
    pub warp: Option<Warp>,
    pub flip: bool,
    /// Which of the part's takes played it.
    pub take: usize,
}

/// What `prepare` and `mix` need besides the song.
pub struct Request<'a> {
    pub song: &'a Song,
    /// The parts as the request gave them.
    pub parts: &'a [Value],
    pub options: Options,
    pub default_corpus: &'a str,
    pub banks: &'a Banks,
    pub env: &'a Env<'a>,
    pub store: &'a AnalysisStore,
    /// Audition one part on a few notes instead of its own: `(part id, MIDI notes)`.
    pub audition: Option<(String, Vec<u8>)>,
    /// Audio only: no tiles, no picture.
    pub audio_only: bool,
    /// Where in the song to start (audio only).
    pub start_seconds: f64,
    /// Play and recommend only this part (the song still decides how a chip switch routes the others).
    pub only_part: Option<String>,
}

/// A tile of the picture.
#[derive(Clone, Debug)]
pub struct Tile {
    pub id: String,
    /// In the machine's own canvas.
    pub cell: Cell,
    /// In the 1920x1080 output.
    pub out_cell: Cell,
}

/// Everything decided before a note is played.
pub struct Prepared {
    pub song: Song,
    pub opts: Options,
    pub settings: HashMap<String, PartSettings>,
    pub chip_parts: HashSet<String>,
    pub sample_of: HashMap<String, Vec<Arc<Sample>>>,
    /// Parts to play, in song order.
    pub playing: Vec<String>,
    /// Parts to play or to show, in song order.
    pub wanted: Vec<String>,
    pub problems: Vec<Value>,
    pub t_from: f64,
    pub t_end: f64,
    pub n_frames: usize,
    pub cuts: Option<HashMap<(String, u64, u8), f64>>,
    pub tiles: Vec<Tile>,
    /// The canvas size in the machine's own pixels.
    pub canvas: (usize, usize),
    pub audio_only: bool,
}

impl Prepared {
    pub fn spf() -> usize {
        SR as usize / FPS
    }

    pub fn frames(&self) -> usize {
        self.n_frames * Self::spf()
    }
}

/// A private score of a few notes on one part; the cached song is never mutated.
fn audition_song(song: &Song, part_id: &str, pitches: &[u8], speed: f64) -> Result<Song> {
    let Some(part) = song.part(part_id) else { return msg("No such instrument to audition.") };
    let mut part = part.clone();
    part.notes = pitches
        .iter()
        .enumerate()
        .map(|(i, p)| Note { start: i as f64 * 2.0 * speed, dur: 0.65 * speed, pitch: *p, velocity: 100 })
        .collect();
    let mut s = song.clone();
    s.duration = (pitches.len().saturating_sub(1) as f64 * 2.0 + 0.65) * speed;
    s.parts = vec![part];
    Ok(s)
}

/// Run `f` over `0..n` on a few threads, results in order.
fn parallel<T: Send>(n: usize, workers: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let out: Mutex<Vec<Option<T>>> = Mutex::new((0..n).map(|_| None).collect());
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..workers.clamp(1, n.max(1)) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= n {
                    return;
                }
                let v = f(i);
                out.lock().unwrap()[i] = Some(v);
            });
        }
    });
    out.into_inner().unwrap().into_iter().map(|v| v.expect("every index ran")).collect()
}

pub(crate) fn threads() -> usize {
    std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(1, 8)
}

fn hit_of(part: &Part, s: &PartSettings) -> Option<String> {
    if part.is_drums() && s.hit {
        part.drum_group.clone()
    } else {
        None
    }
}

/// Validate the request's parts: every one gets a voice that is installed.
fn given_parts(req: &Request) -> Result<HashMap<String, Value>> {
    let mut given = HashMap::new();
    for p in req.parts {
        let Some(obj) = p.as_object() else { continue };
        let Some(id) = obj.get("id").and_then(Value::as_str) else { continue };
        let mut chosen = obj.clone();
        if !req.options.synth {
            let corpus = match obj.get("corpus") {
                None | Some(Value::Null) => req.default_corpus.to_string(),
                Some(Value::String(c)) if !c.trim().is_empty() => c.clone(),
                Some(_) => return msg("Choose an installed corpus for this voice."),
            };
            if !req.banks.contains_key(&corpus) {
                return msg(format!("The corpus '{corpus}' is not installed. Choose another voice."));
            }
            chosen.insert("corpus".into(), Value::String(corpus));
        }
        given.insert(id.to_string(), Value::Object(chosen));
    }
    Ok(given)
}

/// The corpora a request's parts ask for (besides the default).
pub fn corpora_of(parts: &[Value], default_corpus: &str) -> Vec<String> {
    let mut names = vec![default_corpus.to_string()];
    for p in parts {
        if let Some(c) = p.get("corpus").and_then(Value::as_str).filter(|c| !c.trim().is_empty()) {
            if !names.iter().any(|n| n == c) {
                names.push(c.to_string());
            }
        }
    }
    names
}

/// Decide everything before a note is played: settings, sounds, samples,
/// length, and (for video) where each tile goes.
pub fn prepare(req: &Request, progress: Progress) -> Result<Prepared> {
    let opts = req.options.clone();
    let source_song = req.song;
    let speed = opts.speed;
    let mut song = source_song.clone();
    let mut start_seconds = req.start_seconds;
    if let Some((part, pitches)) = &req.audition {
        song = audition_song(source_song, part, pitches, speed)?;
        start_seconds = 0.0;
    }
    if let Some(only) = &req.only_part {
        song.parts.retain(|p| &p.id == only);
    }
    let given = given_parts(req)?;
    let null = Value::Null;
    let chip = opts.chip.as_deref();

    // Recommendations only for parts the request did not choose a sound for,
    // and none at all when nothing is going to be said.
    let needs_voice = |p: &Part| -> bool {
        let asked = given.get(&p.id).unwrap_or(&null);
        let asked_tone = asked.get("tone").and_then(Value::as_str);
        let tone = if ytp_music::channels::overrides_switch(asked_tone) {
            asked_tone.and_then(resolve).unwrap_or_default()
        } else if let Some(c) = chip {
            chip_tone(p, c, false, None, true)
        } else {
            "clean".to_string()
        };
        !synth_tones().contains(&tone)
    };
    let mut recs: HashMap<String, Rec> = HashMap::new();
    if !opts.synth {
        let mut groups: Vec<(String, Vec<Part>)> = Vec::new();
        for p in &song.parts {
            let asked = given.get(&p.id).unwrap_or(&null);
            let muted_hidden = truthy(asked.get("mute")) && !asked.get("visible").is_none_or(|v| truthy(Some(v)));
            if !needs_voice(p) || truthy(asked.get("text")) || muted_hidden {
                continue;
            }
            let slug = asked.get("corpus").and_then(Value::as_str).unwrap_or(req.default_corpus).to_string();
            match groups.iter_mut().find(|(s, _)| *s == slug) {
                Some((_, v)) => v.push(p.clone()),
                None => groups.push((slug, vec![p.clone()])),
            }
        }
        for (slug, parts) in groups {
            let bank = &req.banks[&slug];
            let mut one = song.clone();
            one.parts = parts;
            for (id, rec) in recommend(&one, bank, req.env, req.store, progress)? {
                recs.insert(id, rec);
            }
        }
    }
    let mut settings: HashMap<String, PartSettings> = HashMap::new();
    for p in &song.parts {
        let rec = recs.remove(&p.id).unwrap_or_else(|| Rec::empty(p));
        let defaults = default_settings_for(p, &rec, req.default_corpus);
        settings.insert(p.id.clone(), merge(&defaults, given.get(&p.id)));
    }
    through_chip(source_song, &mut settings, &given, chip, opts.g_transpose, opts.synth);
    let chip_parts: HashSet<String> =
        song.parts.iter().filter(|p| synth_tones().contains(&settings[&p.id].tone)).map(|p| p.id.clone()).collect();

    let t_from = if req.audio_only { num_f(Some(&json!(start_seconds)), 0.0, 0.0, (song.duration / speed).max(0.0)) } else { 0.0 };
    let t_end = (song.duration / speed + TAIL).min(t_from + opts.max_seconds);
    let n_frames = (((t_end - t_from) * FPS as f64).round_ties_even() as usize).max(1);

    let plays = |p: &&Part| settings[&p.id].text.is_some() && !settings[&p.id].mute && !p.notes.is_empty();
    let playing: Vec<&Part> = song.parts.iter().filter(plays).collect();
    // No more notes at once than the machine had channels for.
    let cuts = if opts.accurate {
        let pairs: Vec<(&Part, &str)> = playing.iter().map(|p| (*p, settings[&p.id].tone.as_str())).collect();
        Some(allocate(&pairs, speed))
    } else {
        None
    };
    let shown: Vec<&Part> = if req.audio_only {
        Vec::new()
    } else {
        song.parts
            .iter()
            .filter(|p| settings[&p.id].text.is_some() && settings[&p.id].visible && !p.notes.is_empty())
            .collect()
    };
    let mut wanted: Vec<&Part> = playing.clone();
    for p in &shown {
        if !wanted.iter().any(|w| w.id == p.id) {
            wanted.push(p);
        }
    }
    let wanted_ids: Vec<String> = wanted.iter().map(|p| p.id.clone()).collect();
    let playing_ids: Vec<String> = playing.iter().map(|p| p.id.clone()).collect();

    // Samples first: the cheapest place to find out a word cannot be said.
    struct Want {
        part: usize,
        corpus: String,
        text: String,
        take: usize,
        hit: Option<String>,
    }
    let mut wants: Vec<Want> = Vec::new();
    for (i, part) in wanted.iter().enumerate() {
        let s = &settings[&part.id];
        if chip_parts.contains(&part.id) {
            continue;
        }
        let text = s.text.clone().unwrap_or_default();
        let list: Vec<(String, usize)> = match opts.vary.as_str() {
            "ultra" => s.pool.iter().take(ULTRA_POOL).cloned().collect(),
            "off" => vec![(text.clone(), s.take)],
            _ => s.takes.iter().map(|t| (text.clone(), *t)).collect(),
        };
        let mut seen = HashSet::new();
        for (t, k) in list {
            if seen.insert((t.clone(), k)) {
                wants.push(Want { part: i, corpus: s.corpus.clone(), text: t, take: k, hit: hit_of(part, s) });
            }
        }
    }
    let total = wanted.len();
    say(progress, "samples", 0, total)?;
    let built = parallel(wants.len(), 4, |k| {
        let w = &wants[k];
        let bank = &req.banks[&w.corpus];
        bank.build(req.env, &w.text, w.take, w.hit.as_deref(), &|| progress("samples", 0, total))
    });
    let mut sample_of: HashMap<String, Vec<Arc<Sample>>> = HashMap::new();
    let mut problems: Vec<Value> = Vec::new();
    let mut by_part: HashMap<usize, (Vec<Arc<Sample>>, Option<Error>)> = HashMap::new();
    for (w, r) in wants.iter().zip(built) {
        let e = by_part.entry(w.part).or_default();
        match r {
            Ok(s) => e.0.push(s),
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(err) => e.1 = Some(err),
        }
    }
    for (i, part) in wanted.iter().enumerate() {
        if chip_parts.contains(&part.id) {
            sample_of.insert(part.id.clone(), Vec::new()); // the chip needs nothing to play from
            continue;
        }
        match by_part.remove(&i) {
            Some((got, _)) if !got.is_empty() => {
                sample_of.insert(part.id.clone(), got);
            }
            other => {
                let err = other.and_then(|o| o.1).map(|e| e.to_string()).unwrap_or_default();
                problems.push(json!({
                    "part": part.id, "name": part.name, "text": settings[&part.id].text, "error": err,
                }));
            }
        }
    }
    say(progress, "samples", total, total)?;
    if sample_of.is_empty() {
        return msg(if opts.synth { "There is nothing to play." } else { "None of the parts has a sound this voice can make." });
    }

    // The tiles: every shown part that has a sound, in the order asked for.
    let mut tiles = Vec::new();
    let mut canvas = (OUT_W, OUT_H);
    if !req.audio_only {
        let mut ids: Vec<String> = shown.iter().filter(|p| sample_of.contains_key(&p.id)).map(|p| p.id.clone()).collect();
        if ids.is_empty() && !opts.synth {
            ids = playing_ids.iter().filter(|id| sample_of.contains_key(*id)).take(1).cloned().collect();
        }
        let ids = ordered_ids(ids, opts.tile_order.as_deref());
        let screen_name = opts.screen();
        let (cw, ch) = match screen::size_of(screen_name) {
            Some(sz) => sz,
            None => {
                let cols = (ids.len().max(1) as f64).sqrt().ceil() as usize;
                let cw = OUT_W.min(cols * SRC_W);
                let ch = (2usize).max(cw * OUT_H / OUT_W / 2 * 2);
                if opts.draft {
                    ((2usize).max(cw / 4 * 2), (2usize).max(ch / 4 * 2))
                } else {
                    (cw, ch)
                }
            }
        };
        canvas = (cw, ch);
        let out_cells = grid_cells(ids.len(), OUT_W, OUT_H, &opts.layout, opts.center_tiles);
        for (id, (x, y, w, h)) in ids.into_iter().zip(out_cells) {
            let cell = (x * cw / OUT_W, y * ch / OUT_H, (x + w) * cw / OUT_W - x * cw / OUT_W, (y + h) * ch / OUT_H - y * ch / OUT_H);
            tiles.push(Tile { id, cell, out_cell: (x, y, w, h) });
        }
    }

    Ok(Prepared {
        song: song.clone(),
        opts,
        settings,
        chip_parts,
        sample_of,
        playing: playing_ids,
        wanted: wanted_ids,
        problems,
        t_from,
        t_end,
        n_frames,
        cuts,
        tiles,
        canvas,
        audio_only: req.audio_only,
    })
}

// ── Playing a part ───────────────────────────────────────────────────────────

/// One part's stereo output over the stretch of the song where it sounds.
#[derive(Default)]
pub struct PartBuf {
    base: usize,
    started: bool,
    /// Interleaved stereo.
    data: Vec<f32>,
}

impl PartBuf {
    fn add(&mut self, dest: usize, ch: usize, src: &[f32], gain: f32) {
        if src.is_empty() {
            return;
        }
        if !self.started {
            self.base = dest;
            self.started = true;
        }
        if dest < self.base {
            let extra = self.base - dest;
            self.data.splice(0..0, std::iter::repeat_n(0.0, extra * 2));
            self.base = dest;
        }
        let off = dest - self.base;
        let need = (off + src.len()) * 2;
        if self.data.len() < need {
            self.data.resize(need, 0.0);
        }
        for (i, v) in src.iter().enumerate() {
            self.data[(off + i) * 2 + ch] += v * gain;
        }
    }

    pub fn frames(&self) -> std::ops::Range<usize> {
        self.base..self.base + self.data.len() / 2
    }
}

/// One part's chip notes, with exact keys and a byte limit.
#[derive(Default)]
struct SynthCache {
    notes: Vec<((String, usize, u32, u64, u64, Option<u8>, Option<u8>, bool), Vec<f32>)>,
    bytes: usize,
}

impl SynthCache {
    const BUDGET: usize = 8 * 1024 * 1024;

    fn apply(&mut self, y: &[f32], tone_name: &str, p: &ToneParams) -> Vec<f32> {
        let resolved = resolve(tone_name).unwrap_or_else(|| "clean".into());
        let peak = y.iter().fold(0.0f32, |a, v| a.max(v.abs()));
        if !synth_tones().contains(&resolved) || y.is_empty() || !peak.is_finite() || peak <= 0.0 {
            return tone::apply(y, tone_name, p);
        }
        let key = (
            resolved,
            y.len(),
            peak.to_bits(),
            p.hz.unwrap_or(f64::NAN).to_bits(),
            p.level.to_bits(),
            p.program,
            p.key,
            p.model == Model::Mos8580,
        );
        if let Some(i) = self.notes.iter().position(|(k, _)| *k == key) {
            let entry = self.notes.remove(i);
            let out = entry.1.clone();
            self.notes.push(entry);
            return out;
        }
        let out = tone::apply(y, tone_name, p);
        let size = out.len() * 4;
        if size > 0 && size <= Self::BUDGET {
            while !self.notes.is_empty() && (self.bytes + size > Self::BUDGET || self.notes.len() >= 256) {
                let (_, old) = self.notes.remove(0);
                self.bytes -= old.len() * 4;
            }
            self.bytes += size;
            self.notes.push((key, out.clone()));
        }
        out
    }
}

struct PlayCtx<'a> {
    /// Length of the mix in frames.
    n: usize,
    t_from: f64,
    t_end: f64,
    speed: f64,
    g_transpose: i32,
    flip: bool,
    jitter: bool,
    vary: &'a str,
    cuts: Option<&'a HashMap<(String, u64, u8), f64>>,
}

fn gains(pan: f64) -> (f64, f64) {
    let a = (pan + 1.0) * std::f64::consts::PI / 4.0;
    (a.cos(), a.sin())
}

/// Sing every note of `part` between `t_from` and `t_end` into `buf`; return
/// its hits. `buf` `None` still works out the hits, for a tile that is shown
/// but muted. No takes at all is a part with no voice: the chip plays every
/// note from nothing, and there is no clip to warp.
fn play(
    part: &Part,
    s: &PartSettings,
    takes: &[Arc<Sample>],
    mut buf: Option<&mut PartBuf>,
    c: &PlayCtx,
    tick: &dyn Fn() -> bool,
) -> Result<Vec<Hit>> {
    let (gl, gr) = gains(s.pan);
    let drums = part.is_drums();
    let shift = 12 * s.octave + s.transpose + if drums { 0 } else { c.g_transpose };
    let reference = part.median_pitch() + 12.0 * s.octave as f64; // for a voice with no pitch
    // Seeded on the part, so a render is the same twice: "random" here means
    // unpatterned, not different every time you press the button.
    let mut rng = PyRandom::from_str_seed(&format!("{}|{}|{}", part.id, takes.len(), s.text.as_deref().unwrap_or("None")));
    let mut chip_notes = SynthCache::default();
    let mut notes: Vec<&Note> = part.notes.iter().collect();
    notes.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal).then(a.pitch.cmp(&b.pitch)));
    let mut starts: Vec<f64> = notes.iter().map(|n| n.start).collect();
    starts.dedup();
    let mut hits: Vec<Hit> = Vec::new();
    let mut last_start: Option<f64> = None;
    let mut pick = 0usize;
    let model = if s.chip.as_deref().is_some_and(|c| c.starts_with("sid8580")) { Model::Mos8580 } else { Model::Mos6581 };
    let sr = SR as f64;
    for n in notes {
        if !tick() {
            return Err(Error::Cancelled);
        }
        let t0 = n.start / c.speed;
        if t0 >= c.t_end {
            break;
        }
        if last_start != Some(n.start) {
            // a chord is one hit: one take
            pick = if c.vary == "off" || takes.len() <= 1 {
                0
            } else if c.vary == "random" || c.vary == "ultra" {
                rng.randbelow(takes.len() as u64) as usize
            } else {
                let beat = starts.partition_point(|x| *x < n.start);
                beat % takes.len()
            };
        }
        let j = starts.partition_point(|x| *x <= n.start);
        let nxt = if j < starts.len() { starts[j] / c.speed } else { c.t_end };
        let mut mode = s.mode.as_str();
        let sample = takes.get(pick);
        let ring = s.sustain == "ring";
        let (d, stretch) = match sample {
            None => (if ring { RING_MAX.min(nxt - t0) } else { n.dur / c.speed }, false),
            Some(smp) => {
                if mode == "perfect" && !smp.voice.info.f0.is_some_and(|f| f != 0.0) {
                    mode = "tape";
                }
                if ring {
                    (smp.voice.duration().min(nxt - t0), false)
                } else {
                    (n.dur / c.speed, true)
                }
            }
        };
        let dur = d.min(c.t_end - t0);
        if dur <= 0.0 {
            continue;
        }
        // A drum hit that is identical every beat is a machine gun. A hair of
        // detune (drums only — a pitched part is meant to be exactly in tune)
        // and a decibel either way is what a person hitting something sounds
        // like.
        let detune = if c.jitter && drums { round_to(rng.uniform(-0.25, 0.25), 2) } else { 0.0 };
        let loud = if c.jitter { 10f64.powf(rng.uniform(-1.2, 1.2) / 20.0) } else { 1.0 };
        // Keep the same take/jitter sequence as the full song when seeking,
        // and retain notes whose attack was before the preview cursor.
        let haas_extra = if s.haas { HAAS_DELAY } else { 0.0 };
        if t0 + dur + haas_extra <= c.t_from {
            last_start = Some(n.start);
            continue;
        }
        let midi = n.pitch as i32 + shift;
        let (mut y, warp): (Vec<f32>, Option<Warp>) = match sample {
            None => (vec![0.5f32; ((dur * sr).round_ties_even() as usize).max(1)], None),
            Some(smp) => {
                let v = &smp.voice;
                let has_pitch = v.info.f0.is_some_and(|f| f != 0.0);
                let (y, w) = if drums || mode == "raw" {
                    let mut semis = s.transpose as f64 + detune;
                    if part.drum_group.as_deref() == Some("toms") {
                        // Toms are tuned: GM gives each its own note, low to high.
                        semis += n.pitch as f64 - part.median_pitch().round_ties_even();
                    }
                    v.render(None, dur, Mode::Raw, semis, stretch)
                } else if mode == "tape" && !has_pitch {
                    v.render(None, dur, Mode::Raw, midi as f64 - reference, stretch)
                } else {
                    v.render(Some(midi_to_hz(midi as f64)), dur, Mode::parse(mode), 0.0, stretch)
                };
                (y, Some(w))
            }
        };
        if s.tone != "clean" {
            // A patch synthesises the note, so it has to be told which note —
            // and how hard it was struck. Drums have no note, and say so.
            let p = ToneParams {
                sample_rate: SR,
                hz: if drums { None } else { Some(midi_to_hz(midi as f64)) },
                level: (n.velocity as f64 / 127.0).max(0.2),
                program: s.program.or(part.program),
                key: if drums { Some(n.pitch) } else { None },
                model,
            };
            y = chip_notes.apply(&y, &s.tone, &p);
        }
        let cut = c.cuts.and_then(|m| m.get(&(part.id.clone(), n.start.to_bits(), n.pitch)));
        if let Some(cut) = cut {
            let keep = ((cut - t0) * sr).round_ties_even() as i64;
            if keep <= 0 {
                continue; // never got a channel
            }
            y.truncate(keep as usize);
            let fade = y.len().min((0.004 * sr) as usize);
            if fade > 0 {
                let ramp = ytp_music::np::linspace_f32(1.0, 0.0, fade);
                let at = y.len() - fade;
                for (v, r) in y[at..].iter_mut().zip(ramp) {
                    *v *= r;
                }
            }
        }
        if let (Some(buf), true) = (buf.as_deref_mut(), s.volume > 0.0) {
            let n_frames = c.n as i64;
            let a = ((t0 - c.t_from) * sr).round_ties_even() as i64;
            let gain = s.volume * loud * (0.35 + 0.65 * n.velocity as f64 / 127.0);
            let (gl32, gr32) = ((gain * gl) as f32, (gain * gr) as f32);
            let delay = (HAAS_DELAY * sr).round_ties_even() as i64;
            let mut put = |ch: usize, offset: i64, count_max: i64, g: f32| {
                // y[source..] lands at max(0, offset)
                let dest = offset.max(0);
                let source = (-offset).max(0);
                let count = (y.len() as i64 - source).min(n_frames - dest).min(count_max);
                if count > 0 {
                    buf.add(dest as usize, ch, &y[source as usize..(source + count) as usize], g);
                }
            };
            if a < 0 {
                let left = a + if s.pan > 0.0 && s.haas { delay } else { 0 };
                let right = a + if s.pan <= 0.0 && s.haas { delay } else { 0 };
                put(0, left, i64::MAX, gl32);
                put(1, right, i64::MAX, gr32);
            } else {
                let k = (y.len() as i64).min(n_frames - a);
                if k > 0 {
                    if s.haas {
                        let kd = k.min((n_frames - (a + delay)).max(0));
                        if s.pan <= 0.0 {
                            put(0, a, k, gl32);
                            put(1, a + delay, kd, gr32);
                        } else {
                            put(0, a + delay, kd, gl32);
                            put(1, a, k, gr32);
                        }
                    } else {
                        put(0, a, k, gl32);
                        put(1, a, k, gr32);
                    }
                }
            }
        }
        if last_start != Some(n.start) {
            hits.push(Hit { start: t0, dur, warp, flip: c.flip && hits.len() % 2 == 1, take: pick });
            last_start = Some(n.start);
        }
    }
    Ok(hits)
}

// ── Mixing ───────────────────────────────────────────────────────────────────

/// The finished audio and what the picture needs from it.
pub struct Mixed {
    /// Mastered, interleaved stereo at [`SR`].
    pub stereo: Vec<f32>,
    pub hits: HashMap<String, Vec<Hit>>,
    /// Oscilloscope traces of the chip parts shown as tiles.
    pub traces: HashMap<String, Vec<f32>>,
    pub levels: HashMap<String, Value>,
}

struct PartOut {
    hits: Vec<Hit>,
    buf: Option<PartBuf>,
    trace: Option<Vec<f32>>,
}

/// Play every part into the mix, balance them, and master the result.
pub fn mix(prep: &Prepared, progress: Progress) -> Result<Mixed> {
    let opts = &prep.opts;
    let n = prep.frames();
    let parts: Vec<&Part> = prep.wanted.iter().filter(|id| prep.sample_of.contains_key(*id)).filter_map(|id| prep.song.part(id)).collect();
    let playing: HashSet<&str> = prep.playing.iter().map(String::as_str).collect();
    let total: usize = parts.iter().map(|p| p.notes.len()).sum();
    let done = AtomicUsize::new(0);
    let stopped = AtomicBool::new(false);
    let tick = || -> bool {
        let d = done.fetch_add(1, Ordering::Relaxed) + 1;
        if d.is_multiple_of(200) && !progress("notes", d, total) {
            stopped.store(true, Ordering::Relaxed);
        }
        !stopped.load(Ordering::Relaxed)
    };
    say(progress, "notes", 0, total)?;
    let tile_of: HashMap<&str, &Tile> = prep.tiles.iter().map(|t| (t.id.as_str(), t)).collect();
    let ctx = PlayCtx {
        n,
        t_from: prep.t_from,
        t_end: prep.t_end,
        speed: opts.speed,
        g_transpose: opts.g_transpose,
        flip: opts.flip,
        jitter: opts.jitter,
        vary: &opts.vary,
        cuts: prep.cuts.as_ref(),
    };
    let outs: Vec<Result<PartOut>> = parallel(parts.len(), threads(), |i| {
        let part = parts[i];
        let s = &prep.settings[&part.id];
        let takes = &prep.sample_of[&part.id];
        let is_chip = prep.chip_parts.contains(&part.id);
        let is_playing = playing.contains(part.id.as_str());
        // A chip part shown as a tile needs its sound to draw it, muted or not.
        let wants_trace = is_chip && !prep.audio_only && tile_of.contains_key(part.id.as_str());
        let mut buf = (is_playing || wants_trace).then(PartBuf::default);
        let hits = play(part, s, takes, buf.as_mut(), &ctx, &tick)?;
        let trace = if wants_trace {
            let tile = tile_of[part.id.as_str()];
            let b = buf.as_ref().unwrap();
            let mut mono = vec![0.0f32; n];
            let r = b.frames();
            for (k, f) in r.clone().enumerate() {
                if f < n {
                    mono[f] = (b.data[k * 2] + b.data[k * 2 + 1]) / 2.0;
                }
            }
            let hz = if part.is_drums() {
                None
            } else {
                Some(midi_to_hz(part.median_pitch() + 12.0 * s.octave as f64 + s.transpose as f64 + opts.g_transpose as f64))
            };
            Some(crate::scope::traces(&mono, prep.n_frames, Prepared::spf(), tile.cell.2.saturating_sub(1).max(1), hz))
        } else {
            None
        };
        Ok(PartOut { hits, buf: if is_playing { buf } else { None }, trace })
    });
    if stopped.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }

    let mut hits = HashMap::new();
    let mut traces = HashMap::new();
    let mut bufs: Vec<(&Part, PartBuf)> = Vec::new();
    for (part, out) in parts.iter().zip(outs) {
        let out = out?;
        hits.insert(part.id.clone(), out.hits);
        if let Some(t) = out.trace {
            traces.insert(part.id.clone(), t);
        }
        if let Some(b) = out.buf {
            bufs.push((part, b));
        }
    }
    say(progress, "notes", total, total)?;

    let mut stereo = vec![0.0f32; n * 2];
    let mut levels: HashMap<String, Value> = HashMap::new();
    if opts.balance && !bufs.is_empty() {
        // Every part is a stem until all of them are measured, because where
        // one belongs depends on the rest.
        struct Stem {
            id: String,
            pan: (f64, f64),
            lo: usize,
            mono: Vec<f32>,
        }
        let measured: Vec<(Stem, master::PartPower)> = parallel(bufs.len(), threads(), |i| {
            let (part, b) = &bufs[i];
            let s = &prep.settings[&part.id];
            let (gl, gr) = gains(s.pan);
            let denom = if (gl + gr).abs() > 1e-6 { (gl + gr) as f32 } else { 1.0 };
            let lo = b.base;
            let mono: Vec<f32> = b.data.as_chunks::<2>().0.iter().map(|f| (f[0] + f[1]) / denom).collect();
            let power = master::block_power_span(&mono, lo, n, SR);
            // What the person asked for over and above the old table, kept:
            // the measurement decides where a part sits on its own, and
            // somebody who has moved a slider still outranks it.
            let reference = default_volume(&part.role);
            let trim_db = if reference > 0.0 { 20.0 * (s.volume.max(1e-4) / reference).log10() } else { 0.0 };
            (
                Stem { id: part.id.clone(), pan: (gl, gr), lo, mono },
                master::PartPower { id: part.id.clone(), role: part.role.clone(), power, trim_db },
            )
        });
        let powers: Vec<master::PartPower> = measured.iter().map(|(_, p)| p.clone()).collect();
        let placed = master::balance(&powers);
        let chunk = 60 * SR as usize;
        for (stem, _) in &measured {
            let b = &placed[&stem.id];
            let pair = (stem.pan.0 as f32 * b.gain as f32, stem.pan.1 as f32 * b.gain as f32);
            let hi = stem.lo + stem.mono.len();
            let mut i = stem.lo;
            while i < hi {
                let j = (i + chunk).min(hi);
                let env = master::envelope(&b.ride, i, j, SR);
                for (k, e) in env.iter().enumerate() {
                    let m = stem.mono[i - stem.lo + k] * e;
                    stereo[(i + k) * 2] += m * pair.0;
                    stereo[(i + k) * 2 + 1] += m * pair.1;
                }
                i = j;
            }
            levels.insert(
                stem.id.clone(),
                json!({
                    "lufs": if b.lufs.is_finite() { json!(round_to(b.lufs, 1)) } else { Value::Null },
                    "gain_db": round_to(20.0 * b.gain.max(1e-6).log10(), 1),
                }),
            );
        }
    } else {
        for (_, b) in &bufs {
            for (k, f) in b.data.as_chunks::<2>().0.iter().enumerate() {
                let at = b.base + k;
                if at < n {
                    stereo[at * 2] += f[0];
                    stereo[at * 2 + 1] += f[1];
                }
            }
        }
    }

    // Master in float64: loudness gate, gain, lookahead limiter.
    let mut m64: Vec<f64> = stereo.iter().map(|v| *v as f64).collect();
    drop(stereo);
    master::finish(&mut m64, 2, SR);
    let stereo: Vec<f32> = m64.iter().map(|v| *v as f32).collect();
    Ok(Mixed { stereo, hits, traces, levels })
}

impl Mixed {
    /// 16-bit stereo WAV bytes of the mix.
    pub fn wav(&self) -> Vec<u8> {
        let samples = wav::quantize_f32(&self.stereo);
        wav::write(&wav::Wav { sample_rate: SR, channels: 2, samples })
    }
}

/// What a render or preview reports about each part.
pub fn parts_report(prep: &Prepared, mixed: &Mixed) -> Vec<Value> {
    prep.song
        .parts
        .iter()
        .map(|p| {
            let s = &prep.settings[&p.id];
            let (poly, typical) = p.polyphony();
            let samples = prep.sample_of.get(&p.id);
            let mut v = json!({
                "id": p.id, "name": p.name, "role": p.role,
                "corpus": s.corpus, "text": s.text, "take": s.take, "octave": s.octave, "transpose": s.transpose,
                "mode": s.mode, "tone": s.tone, "program": s.program, "mute": s.mute, "visible": s.visible,
                "pan": s.pan, "haas": s.haas,
                "polyphony": poly, "typical_polyphony": round_to(typical, 2),
                "played": samples.map(|v| v.iter().map(|x| x.text.clone()).collect::<Vec<_>>()).unwrap_or_default(),
                "takes_played": samples.map_or(0, Vec::len),
                "pitch": samples.and_then(|v| v.first()).map(|x| x.describe()),
            });
            if let (Some(lv), Some(o)) = (mixed.levels.get(&p.id), v.as_object_mut()) {
                if let Some(l) = lv.as_object() {
                    o.extend(l.clone());
                }
            }
            v
        })
        .collect()
}

/// The `parts` list of a preview: what played and how.
pub fn playing_report(prep: &Prepared) -> Vec<Value> {
    prep.song
        .parts
        .iter()
        .filter(|p| prep.playing.contains(&p.id) && prep.sample_of.contains_key(&p.id))
        .map(|p| {
            let s = &prep.settings[&p.id];
            json!({ "id": p.id, "corpus": s.corpus, "volume": s.volume, "pan": s.pan })
        })
        .collect()
}

/// A few seconds of one part, alone, as it will sound — audio only.
///
/// Starts at the part's first note rather than the top of the song, since a
/// part that comes in at bar 17 would otherwise preview as silence.
pub fn preview_part(req: &Request, part_settings: &Value, seconds: f64, progress: Progress) -> Result<(Vec<u8>, Value)> {
    let song = req.song;
    let Some(id) = part_settings.get("id").and_then(Value::as_str) else { return msg("No such part.") };
    let Some(part) = song.part(id) else { return msg("No such part.") };
    let first = part.notes.first().map_or(0.0, |n| n.start);
    let t_from = (first - 0.05).max(0.0);
    // The part alone, on its own settings, at full level and centred.
    let mut one = part_settings.clone();
    if let Some(o) = one.as_object_mut() {
        o.insert("volume".into(), json!(1.0));
        o.insert("pan".into(), json!(0.0));
        o.insert("mute".into(), json!(false));
        o.insert("visible".into(), json!(false));
    }
    let mut opts = req.options.clone();
    opts.balance = false;
    opts.max_seconds = seconds;
    opts.speed = 1.0;
    opts.g_transpose = 0;
    opts.flip = false;
    let parts = [one];
    let sub = Request {
        song,
        parts: &parts,
        options: opts,
        default_corpus: req.default_corpus,
        banks: req.banks,
        env: req.env,
        store: req.store,
        audition: None,
        audio_only: true,
        start_seconds: t_from,
        only_part: Some(id.to_string()),
    };
    let prep = prepare(&sub, progress)?;
    if !prep.playing.contains(&id.to_string()) {
        return msg("Give the part a word first.");
    }
    let mixed = mix(&prep, progress)?;
    let settings = prep.settings[id].to_json();
    let sample = prep.sample_of.get(id).and_then(|v| v.first()).map(|s| s.describe());
    let info = json!({ "from": round_to(prep.t_from, 2), "settings": settings, "sample": sample });
    Ok((mixed.wav(), info))
}

/// Options for a mix preview, held to the ranges preview-mix allows.
pub fn preview_options(options: &Value, seconds: Option<f64>, keyboard: Option<usize>) -> Options {
    let mut o = options.clone();
    if !o.is_object() {
        o = json!({});
    }
    let m = o.as_object_mut().unwrap();
    m.insert(
        "max_seconds".into(),
        json!(match (keyboard, seconds) {
            (Some(k), _) => (k * 2) as f64,
            (None, None) => MAX_SECONDS,
            (None, Some(s)) => s.clamp(1.0, MAX_SECONDS),
        }),
    );
    m.insert("flip".into(), json!(false));
    Options::from_json(&o, MAX_SECONDS)
}

/// A summary of what a render played, for the job record.
pub fn render_summary(prep: &Prepared, mixed: &Mixed) -> Value {
    let song_meta = crate::song::summary_without_parts(&prep.song);
    let mut options = prep.opts.to_json();
    if let Some(o) = options.as_object_mut() {
        o.insert("tile_order".into(), json!(prep.tiles.iter().map(|t| t.id.clone()).collect::<Vec<_>>()));
    }
    json!({
        "duration": round_to(prep.t_end - prep.t_from, 2),
        "size": [OUT_W, OUT_H],
        "song": song_meta,
        "options": options,
        "parts": parts_report(prep, mixed),
        "problems": prep.problems,
    })
}
