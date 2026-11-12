//! A word or phrase, said by the corpus, ready to be played as an instrument
//! (`app/ytpmv/samples.py`).
//!
//! A single word has *takes* — every usable recording of it — and take N is
//! always the same recording, so a choice made in the page survives to the
//! render. Phrases, splices and marked-up words go through the sentence
//! machinery (`resolve_text` and the renderer) with a fixed seed, so a
//! preview is what gets rendered. What is added for an instrument is only the
//! dead air trimmed off the front so a note lands on the beat, the pitch
//! measured, and the picture decoded to frames for the grid.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use ytp_media::{RenderJob, Renderer};
use ytp_music::drums;
use ytp_music::pitch::Voice;
use ytp_splice::select::{duration_floor, vetoed};
use ytp_splice::window::{extract_window, PAD_END};
use ytp_splice::{resolve_text, AudioCache, Clip, GenerationOptions, Pool, PyRandom, Runtime, Segment};

use crate::decode::{decode_audio, decode_video, Frames};
use crate::{msg, round_to, Error, Result, SR};

/// A sample starts where it first reaches this share of its peak ...
const LEAD_FLOOR: f64 = 0.04;
const TAIL_FLOOR: f64 = 0.02;
/// ... less a few ms, so the attack itself is not shaved.
const LEAD_KEEP: f64 = 0.004;
/// A phrase longer than this is not an instrument, it is a speech.
const MAX_SAMPLE: f64 = 3.0;
const CACHE_MAX: usize = 48;
const UNITS_KEPT: usize = 60;

pub const VOWELS: &[&str] = &[
    "aa", "ae", "ah", "ao", "aw", "ay", "eh", "er", "ey", "ih", "iy", "ow", "oy", "uh", "uw",
];
pub const FRICATIVES: &[&str] = &["s", "sh", "z", "zh", "f", "v", "th", "dh", "hh"];
pub const PLOSIVES: &[&str] = &["p", "b", "t", "d", "k", "g", "ch", "jh"];

/// How long each kind of sound ought to be: (shortest, longest, the length to prefer).
fn shape_of(kind: &str) -> (f64, f64, f64) {
    match kind {
        "vowel" => (0.07, 0.60, 0.20),
        "fricative" => (0.05, 0.40, 0.14),
        "plosive" => (0.03, 0.20, 0.07),
        _ => (0.05, 0.35, 0.12),
    }
}

fn kind_of(name: &str) -> &'static str {
    if VOWELS.contains(&name) {
        "vowel"
    } else if FRICATIVES.contains(&name) {
        "fricative"
    } else if PLOSIVES.contains(&name) {
        "plosive"
    } else {
        "other"
    }
}

/// How the pieces of the instrument environment are reached.
pub struct Env<'a> {
    pub ffmpeg: &'a Path,
    /// Cuts and encodes the picture of a sample.
    pub renderer: &'a dyn Renderer,
    /// Where sample videos are kept.
    pub samples_dir: &'a Path,
}

/// One recording to play: a corpus clip, or a phoneme cut out of one.
#[derive(Clone, Debug)]
pub struct Take {
    pub clip: Clip,
    pub fade_in: Option<f64>,
    pub fade_out: Option<f64>,
    /// The clip id (`"123"`, or `"123#4"` for the fifth sound of clip 123).
    pub key_id: String,
}

impl Take {
    fn of(clip: &Clip) -> Take {
        Take { key_id: clip.id.map(|i| i.to_string()).unwrap_or_default(), clip: clip.clone(), fade_in: None, fade_out: None }
    }

    pub fn segment(&self) -> Segment {
        let mut s = Segment::from_clip(&self.clip);
        s.fade_in = self.fade_in;
        s.fade_out = self.fade_out;
        s
    }

    /// The key the recommendation cache stores a measurement under.
    pub fn cache_key(&self) -> String {
        format!(
            "{}:{:.3}:{:.3}",
            if self.key_id.is_empty() { "None" } else { &self.key_id },
            self.clip.start_time,
            self.clip.end_time
        )
    }
}

pub fn norm(text: &str) -> String {
    text.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `/ah/` → `ah`
fn parse_unit(text: &str) -> Option<&str> {
    let inner = text.strip_prefix('/')?.strip_suffix('/')?;
    let letters = inner.trim_end_matches(|c: char| c.is_ascii_digit());
    let digits = inner.len() - letters.len();
    ((1..=3).contains(&letters.len()) && letters.bytes().all(|b| b.is_ascii_lowercase()) && digits <= 1).then_some(inner)
}

/// `*spew*` → `spew`
fn parse_noise(text: &str) -> Option<&str> {
    let inner = text.strip_prefix('*')?.strip_suffix('*')?;
    (!inner.is_empty() && inner.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')).then_some(inner)
}

/// Where a sample's sound starts and stops: from its first moment of real
/// level, to the last, without shaving the attack.
pub fn trim(x: &[f32]) -> (usize, usize) {
    if x.is_empty() {
        return (0, 0);
    }
    let hop = (0.005 * SR as f64) as usize;
    if x.len() < hop {
        return (0, x.len());
    }
    let n = (x.len() / hop).max(1);
    let env: Vec<f64> = (0..n)
        .map(|k| {
            let s: f64 = x[k * hop..(k + 1) * hop].iter().map(|v| (*v as f64) * (*v as f64)).sum();
            (s / hop as f64).sqrt()
        })
        .collect();
    let peak = env.iter().cloned().fold(0.0f64, f64::max);
    if peak <= 0.0 {
        return (0, x.len());
    }
    let lead = env.iter().position(|e| *e >= LEAD_FLOOR * peak).unwrap_or(0);
    let tail = env.iter().rposition(|e| *e >= TAIL_FLOOR * peak).unwrap_or(0);
    let a = (lead * hop).saturating_sub((LEAD_KEEP * SR as f64) as usize);
    let b = x.len().min((tail + 2) * hop);
    (a, b.max(a + hop))
}

/// A sample: a word (or phrase) at a take, measured and ready to be sung.
pub struct Sample {
    pub text: String,
    pub take: usize,
    /// How many takes the word has (1 for phrases).
    pub takes: usize,
    /// Drum group it was cut into a hit for, if any.
    pub hit: Option<String>,
    mp4: PathBuf,
    /// Seconds of lead-in trimmed from the mp4.
    pub offset: f64,
    pub voice: Voice,
    pub source: String,
    pub corpus: String,
    segments: Vec<Segment>,
}

impl Sample {
    pub fn duration(&self) -> f64 {
        self.voice.duration()
    }

    pub fn describe(&self) -> Value {
        let i = &self.voice.info;
        json!({
            "text": self.text, "take": self.take, "takes": self.takes, "hit": self.hit,
            "corpus": self.corpus, "source": self.source,
            "f0": i.f0.map(|f| round_to(f, 2)), "midi": i.midi.map(|m| round_to(m, 2)), "note": i.note,
            "voiced": round_to(i.voiced_ratio, 3),
            "stability": if i.stability.is_finite() { json!(round_to(i.stability.min(1200.0), 1)) } else { Value::Null },
            "duration": round_to(i.duration, 3),
        })
    }

    /// Make the sample's picture on disk, once.
    fn ensure_video(&self, env: &Env, keep_going: &dyn Fn() -> bool) -> Result<()> {
        if self.mp4.is_file() {
            return Ok(());
        }
        render_segments(env, &self.segments, &self.mp4, keep_going)
    }

    /// The picture at `w`x`h`, one frame per 1/FPS s from the trimmed start.
    pub fn frames(
        &self,
        env: &Env,
        w: usize,
        h: usize,
        cover: bool,
        pixel_aspect: f64,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<Frames> {
        if self.segments.is_empty() {
            return msg("this sample has no picture");
        }
        self.ensure_video(env, keep_going)?;
        decode_video(env.ffmpeg, &self.mp4, self.offset, self.duration(), w, h, cover, pixel_aspect, keep_going)
    }
}

/// Encode `segments` to `out` through the renderer, written aside and moved
/// into place so two builders never interleave.
fn render_segments(env: &Env, segments: &[Segment], out: &Path, keep_going: &dyn Fn() -> bool) -> Result<()> {
    std::fs::create_dir_all(env.samples_dir).map_err(|e| Error::Internal(e.to_string()))?;
    let unique: u64 = rand_u64();
    let tmp = out.with_extension(format!("{unique:x}.tmp.mp4"));
    let audio = AudioCache::new(env.renderer.audio());
    let opts = GenerationOptions::default();
    let job = RenderJob { segments, options: &opts, captions: false, out: &tmp, audio: &audio, max_height: 0, encoder: None, finish: false, cancel: None };
    let progress = |_: &str, _: usize, _: usize| keep_going();
    let res = env.renderer.render(&job, &progress);
    let res = match res {
        Ok(()) => std::fs::rename(&tmp, out).map_err(|e| Error::Internal(e.to_string())),
        Err(ytp_media::RenderError::Cancelled) => Err(Error::Cancelled),
        Err(e) => Err(Error::Msg(e.to_string())),
    };
    let _ = std::fs::remove_file(&tmp);
    res
}

fn rand_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    h.finish()
}

type Key = (String, usize, Option<String>);

/// One corpus as a bank of instruments.
pub struct Bank {
    /// The corpus's name.
    pub name: String,
    pub pool: Arc<Pool>,
    units: OnceLock<BTreeMap<String, Vec<Take>>>,
    cache: Mutex<VecDeque<(Key, Arc<Sample>)>>,
}

impl Bank {
    pub fn new(name: &str, pool: Arc<Pool>) -> Bank {
        Bank { name: name.to_string(), pool, units: OnceLock::new(), cache: Mutex::new(VecDeque::new()) }
    }

    /// Every phoneme this voice has, as clips of its own: `{"ah": [clip, ...]}`.
    ///
    /// Each is a slice of a real clip, marked *edited* so it is cut exactly
    /// where the aligner put the boundary rather than padded like a word.
    pub fn units(&self) -> &BTreeMap<String, Vec<Take>> {
        self.units.get_or_init(|| {
            let mut found: BTreeMap<String, Vec<Take>> = BTreeMap::new();
            for rows in self.pool.by_word.values() {
                for &ci in rows {
                    let clip = &self.pool.clips[ci];
                    let Some(phones) = &clip.phones else { continue };
                    for (i, (ph, a, b)) in phones.iter().enumerate() {
                        let name = ph.to_lowercase().trim_end_matches(|c: char| c.is_ascii_digit()).to_string();
                        let (lo, hi, _) = shape_of(kind_of(&name));
                        let dur = b - a;
                        if !(lo <= dur && dur <= hi) {
                            continue;
                        }
                        let start = clip.start_time + a.max(0.0);
                        let end = clip.start_time + b;
                        let unit = Clip {
                            id: None,
                            source_id: clip.source_id,
                            word: format!("/{name}/"),
                            start_time: start,
                            end_time: end,
                            source_file: clip.source_file.clone(),
                            prev_end: Some(start),
                            next_start: Some(end),
                            bleed_risk: false,
                            edited: true,
                            tone: None,
                            phones: None,
                        };
                        found.entry(name).or_default().push(Take {
                            clip: unit,
                            // A word's fades would swallow a 60ms sound whole.
                            fade_in: Some(0.004),
                            fade_out: Some(0.006),
                            key_id: format!("{}#{i}", clip.id.map(|x| x.to_string()).unwrap_or_default()),
                        });
                    }
                }
            }
            for (name, rows) in found.iter_mut() {
                let want = shape_of(kind_of(name)).2;
                rows.sort_by(|x, y| {
                    let dx = ((x.clip.end_time - x.clip.start_time) - want).abs();
                    let dy = ((y.clip.end_time - y.clip.start_time) - want).abs();
                    dx.partial_cmp(&dy).unwrap_or(std::cmp::Ordering::Equal)
                });
                rows.truncate(UNITS_KEPT);
            }
            found
        })
    }

    /// Every non-verbal noise the voice has, and how many recordings of each.
    pub fn noises(&self) -> Vec<(String, usize)> {
        let mut found: Vec<(String, usize)> =
            self.pool.noise_by_word.iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| (k.clone(), v.len())).collect();
        if self.pool.noises.len() > 1 && found.len() > 1 {
            found.push(("noise".to_string(), self.pool.noises.len()));
        }
        found.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        found
    }

    fn noise_takes(&self, kind: &str) -> Vec<Take> {
        let rows: &[usize] = if kind == "noise" {
            &self.pool.noises
        } else {
            self.pool.noise_by_word.get(kind).map(|v| v.as_slice()).unwrap_or(&[])
        };
        let mut takes: Vec<Take> = rows.iter().map(|&i| Take::of(&self.pool.clips[i])).collect();
        takes.sort_by(|a, b| {
            (a.clip.id.unwrap_or(0), a.clip.start_time)
                .partial_cmp(&(b.clip.id.unwrap_or(0), b.clip.start_time))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        takes
    }

    /// Every recording of `word` worth playing, in a fixed order: the band
    /// `pick_clip` uses (a clip far shorter or longer than the word's median
    /// is a bad timestamp, not a take) and the same vetoes.
    fn word_takes(&self, word: &str) -> Vec<Take> {
        let Some(idx) = self.pool.by_word.get(word) else { return Vec::new() };
        let mut rows: Vec<usize> = idx.iter().copied().filter(|&i| !vetoed(&self.pool, word, i)).collect();
        if rows.len() > 1 {
            let dur = |i: usize| self.pool.clips[i].duration();
            let mut durs: Vec<f64> = rows.iter().map(|&i| dur(i)).collect();
            durs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let median = durs[durs.len() / 2];
            let floor = duration_floor(word).max(0.5 * median);
            let ceiling = (2.5 * median).max(0.45);
            let good: Vec<usize> = rows.iter().copied().filter(|&i| floor <= dur(i) && dur(i) <= ceiling).collect();
            if !good.is_empty() {
                rows = good;
            }
        }
        let mut takes: Vec<Take> = rows.iter().map(|&i| Take::of(&self.pool.clips[i])).collect();
        takes.sort_by(|a, b| {
            (a.clip.id.unwrap_or(0), a.clip.start_time)
                .partial_cmp(&(b.clip.id.unwrap_or(0), b.clip.start_time))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        takes
    }

    /// The takes behind `text`: a phoneme, a noise, or a word.
    pub fn takes_for(&self, text: &str) -> Vec<Take> {
        let text = norm(text);
        if let Some(u) = parse_unit(&text) {
            return self.units().get(u).cloned().unwrap_or_default();
        }
        if let Some(n) = parse_noise(&text) {
            return self.noise_takes(n);
        }
        self.word_takes(&text)
    }

    pub fn clip_count(&self, word: &str) -> usize {
        self.pool.by_word.get(word).map_or(0, Vec::len)
    }

    /// The corpus saying `text` (take `take`, for a single word) as a sample.
    ///
    /// `hit` names a drum (kick, snare, hats...) to cut the word down into,
    /// the way a beatboxer would. Errors with a message the person can read
    /// when the voice cannot say it.
    pub fn build(
        &self,
        env: &Env,
        text: &str,
        take: usize,
        hit: Option<&str>,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<Arc<Sample>> {
        if !keep_going() {
            return Err(Error::Cancelled);
        }
        let text = norm(text);
        if text.is_empty() {
            return msg("Nothing to say.");
        }
        let hit: Option<String> = hit.map(|h| if drums::GROUPS.contains(&h) { h.to_string() } else { "perc".to_string() });
        let key: Key = (text.clone(), take, hit.clone());
        {
            let mut c = self.cache.lock().unwrap();
            if let Some(pos) = c.iter().position(|(k, _)| *k == key) {
                let entry = c.remove(pos).unwrap();
                let s = entry.1.clone();
                c.push_back(entry);
                return Ok(s);
            }
        }

        let word_takes: Vec<Take> = if let Some(u) = parse_unit(&text) {
            let t = self.units().get(u).cloned().unwrap_or_default();
            if t.is_empty() {
                return msg(format!("This voice has no {text} sound the aligner could find."));
            }
            t
        } else if let Some(n) = parse_noise(&text) {
            let t = self.noise_takes(n);
            if t.is_empty() {
                return msg(format!("This voice has no {n} noise."));
            }
            t
        } else if !text.contains([' ', '~', '*']) {
            self.word_takes(&text)
        } else {
            Vec::new()
        };
        let n_takes = word_takes.len().max(1);
        let audio_probe = AudioCache::new(env.renderer.audio());

        let (segments, source, x): (Vec<Segment>, String, Vec<f32>) = if !word_takes.is_empty() {
            let t = &word_takes[take % word_takes.len()];
            let seg = t.segment();
            let source = format!(
                "{} @ {:.2}s",
                Path::new(&t.clip.source_file).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                t.clip.start_time
            );
            // The sound straight from the recording, the same window the
            // recommender measured, which the picture also starts at.
            let (s, e) = extract_window(&seg, PAD_END, &audio_probe);
            let x = decode_audio(env.ffmpeg, &seg.source_file, Some(s), Some(e - s), keep_going)
                .ok()
                .filter(|x| !x.is_empty());
            let Some(x) = x else {
                if !keep_going() {
                    return Err(Error::Cancelled);
                }
                return msg(format!("\"{text}\" has no sound in its recording"));
            };
            (vec![seg], source, x)
        } else {
            // A phrase, a splice or a marked-up word: the sentence machinery
            // picks at random, so seed it — the same text and take must give
            // the same sample every time.
            let opts = GenerationOptions::default();
            let mut rng = PyRandom::from_str_seed(&format!("{}|{}|{}", self.name, text, take));
            let rt = Runtime {
                pool: self.pool.clone(),
                voices: &|_| None,
                audio: &audio_probe,
                glitches: Vec::new(),
            };
            let (segments, _report) = resolve_text(&text, &opts, &rt, &mut rng, &|_, _, _| keep_going())
                .map_err(|_| Error::Cancelled)?;
            if segments.is_empty() {
                return msg(format!("This voice can't say \"{text}\"."));
            }
            let source = if segments.len() > 1 { "spliced".to_string() } else { String::new() };
            let mp4 = self.mp4_path(env, &text, take, &segments);
            if !mp4.is_file() {
                render_segments(env, &segments, &mp4, keep_going)?;
            }
            let x = decode_audio(env.ffmpeg, &mp4.to_string_lossy(), None, None, keep_going)
                .ok()
                .filter(|x| !x.is_empty());
            let Some(x) = x else {
                let _ = std::fs::remove_file(&mp4);
                if !keep_going() {
                    return Err(Error::Cancelled);
                }
                return msg(format!("\"{text}\" has no sound in its recording"));
            };
            (segments, source, x)
        };

        let mp4 = self.mp4_path(env, &text, take, &segments);
        let (mut a, b) = trim(&x);
        let b = b.min(a + (MAX_SAMPLE * SR as f64) as usize);
        let mut audio: Vec<f32> = x[a..b].to_vec();
        if let Some(h) = &hit {
            let (shaped, start) = drums::shape(&audio, h);
            audio = shaped;
            a += start;
        }
        let sample = Arc::new(Sample {
            text,
            take: take % n_takes,
            takes: n_takes,
            hit,
            mp4,
            offset: a as f64 / SR as f64,
            voice: Voice::new(&audio, SR),
            source,
            corpus: self.name.clone(),
            segments,
        });
        let mut c = self.cache.lock().unwrap();
        c.push_back((key, sample.clone()));
        while c.len() > CACHE_MAX {
            c.pop_front();
        }
        Ok(sample)
    }

    /// The clip itself is in the name, not just its index: take 3 of a word is
    /// whatever sorts third, and that moves when the corpus is edited.
    fn mp4_path(&self, env: &Env, text: &str, take: usize, segments: &[Segment]) -> PathBuf {
        let mut h = Sha256::new();
        h.update(format!("{}|{}|{}", self.name, text, take));
        for s in segments {
            h.update(format!(
                "|{:?}{}{:.4}{:.4}{:?}{:?}{}{:?}{:?}{:?}{:?}{}",
                s.id, s.source_file, s.start_time, s.end_time, s.prev_end, s.next_start, s.edited, s.fade_in, s.fade_out,
                s.stretch, s.pause_after, s.reverse
            ));
            if let Ok(m) = std::fs::metadata(&s.source_file) {
                h.update(format!("{}{:?}", m.len(), m.modified().ok()));
            }
        }
        let d = h.finalize();
        let hex: String = d.iter().take(6).map(|b| format!("{b:02x}")).collect();
        env.samples_dir.join(format!("ytpmv_sample_{hex}.mp4"))
    }
}

/// Banks by corpus name.
pub type Banks = HashMap<String, Arc<Bank>>;
