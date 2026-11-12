//! Which word should play which part (`app/ytpmv/recommend.py`).
//!
//! Nobody wants to audition 1,400 words to find a kick drum. Each role has
//! words that tend to make the right *kind* of sound — a plosive for a kick,
//! a hiss for a hat, a long open vowel for a bass line — and every recording
//! of those words the corpus actually has is measured and scored for the job:
//!
//! * pitched parts: voiced for most of its length, steady in pitch, long
//!   enough to hold a note
//! * drums: a sharp attack, short, and bright or dark to suit the drum
//!
//! Measurements are cached per corpus on disk, so only the first song on a
//! new voice pays for them. The preferred words are only a starting point: a
//! voice that never said "boom" still gets a kick from whatever it did say
//! that hits hardest.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use indexmap_free::OrderedMap;
use serde_json::{json, Map, Value};
use ytp_music::drums;
use ytp_music::music::{auto_octave, Part, Song};
use ytp_music::np;
use ytp_music::pitch::{describe, track_f0_default};
use ytp_splice::window::{extract_window, PAD_END};
use ytp_splice::AudioCache;

use crate::decode::decode_audio;
use crate::samples::{trim, Bank, Env, Take};
use crate::{round_to, say, Progress, Result, SR};

/// Insertion-ordered map without another dependency.
mod indexmap_free {
    #[derive(Default)]
    pub struct OrderedMap<V> {
        keys: Vec<String>,
        map: std::collections::HashMap<String, V>,
    }

    impl<V> OrderedMap<V> {
        pub fn new() -> Self {
            Self { keys: Vec::new(), map: Default::default() }
        }
        pub fn contains_key(&self, k: &str) -> bool {
            self.map.contains_key(k)
        }
        pub fn insert(&mut self, k: String, v: V) {
            if self.map.insert(k.clone(), v).is_none() {
                self.keys.push(k);
            }
        }
        pub fn get(&self, k: &str) -> Option<&V> {
            self.map.get(k)
        }
        pub fn iter(&self) -> impl Iterator<Item = (&String, &V)> {
            self.keys.iter().map(|k| (k, &self.map[k]))
        }
    }
}

fn preferred(role: &str) -> &'static [&'static str] {
    match role {
        // Drums are cut down to a hit, so what matters is the sound a word
        // *starts* with, or the hiss it has in it — not the word.
        "drums:kick" => &[
            "boom", "bum", "big", "bob", "but", "bad", "bed", "bag", "dad", "dog", "ball", "box", "back", "done", "bang",
        ],
        "drums:snare" => &[
            "kiss", "kids", "cats", "kicks", "chips", "cups", "cakes", "keys", "cheese", "chess", "cat", "kit", "check",
            "chip", "tea", "top", "get", "got",
        ],
        "drums:hats" => &["see", "sit", "this", "yes", "so", "is", "its", "says", "she", "sss", "shh", "cheese", "kiss", "us"],
        "drums:toms" => &["dum", "done", "dog", "bum", "doom", "bong", "down", "dad", "bed", "boom", "good"],
        "drums:cymbals" => &["crash", "splash", "wish", "fish", "shush", "she", "shoes", "sea", "yes", "chips"],
        "drums:perc" => &["tick", "tock", "cup", "pop", "top", "tap", "clap", "click", "kit"],
        "bass" => &["oh", "no", "go", "all", "more", "now", "low", "you", "moo", "boo"],
        "lead" => &["yeah", "me", "oh", "you", "no", "la", "ah", "so", "hi", "my", "why", "wow"],
        "chords" => &["ah", "oh", "all", "you", "more", "now", "me", "no"],
        _ => &["no", "go", "oh", "yeah", "you", "me", "ah", "my", "all"],
    }
}

/// One sound out of the middle of a word, which the aligner has already
/// located in every clip. A vowel on its own holds a note in a way no whole
/// word does; drums get the consonants: a /k/ is a snare hit with nothing
/// else attached to it.
fn unit_words(role: &str) -> &'static [&'static str] {
    match role {
        "bass" => &["/ow/", "/uw/", "/ao/", "/aa/", "/ah/", "/er/", "/oy/"],
        "drums:kick" => &["/b/", "/d/", "/g/", "/p/"],
        "drums:snare" => &["/k/", "/t/", "/ch/", "/p/", "/jh/"],
        "drums:hats" => &["/s/", "/sh/", "/z/", "/f/", "/th/"],
        "drums:toms" => &["/d/", "/b/", "/g/", "/m/", "/n/"],
        "drums:cymbals" => &["/sh/", "/s/", "/zh/", "/ch/"],
        "drums:perc" => &["/t/", "/k/", "/p/", "/ch/"],
        _ => &["/ah/", "/aa/", "/ao/", "/ow/", "/uw/", "/iy/", "/eh/", "/ae/", "/er/", "/ay/", "/aw/"],
    }
}

/// Bumped whenever `measure()` learns something new, so a cache written by an
/// older version is re-measured instead of quietly scoring on missing fields.
const MEASURE_VERSION: i64 = 2;
const FALLBACK_WORDS: usize = 30;
const TAKES_PER_WORD: usize = 4;
const ALTERNATIVES: usize = 12;
const ROTATE: usize = 3;
const POOL: usize = 6;
const SING_TRIES: usize = 6;

/// What a sound is like, in the terms the scores care about.
#[derive(Clone, Debug)]
pub struct Measure {
    pub dur: f64,
    pub f0: Option<f64>,
    pub midi: Option<f64>,
    pub voiced: f64,
    pub stability: f64,
    pub attack: f64,
    pub centroid: f64,
    /// Share of the energy above 4 kHz: hiss.
    pub hf: f64,
    /// ... below 250 Hz: body, what a kick is.
    pub lf: f64,
    /// Peak over average: how much it punches.
    pub crest: f64,
}

impl Measure {
    pub fn to_json(&self) -> Value {
        json!({
            "dur": self.dur, "f0": self.f0, "midi": self.midi, "voiced": self.voiced, "stability": self.stability,
            "attack": self.attack, "centroid": self.centroid, "hf": self.hf, "lf": self.lf, "crest": self.crest,
        })
    }

    fn from_json(v: &Value) -> Option<Measure> {
        let f = |k: &str| v.get(k).and_then(Value::as_f64);
        Some(Measure {
            dur: f("dur")?,
            f0: f("f0"),
            midi: f("midi"),
            voiced: f("voiced")?,
            stability: f("stability")?,
            attack: f("attack")?,
            centroid: f("centroid")?,
            hf: f("hf")?,
            lf: f("lf")?,
            crest: f("crest")?,
        })
    }
}

/// What a sound is like. `x` is trimmed first.
pub fn measure(x: &[f32]) -> Measure {
    let (a, b) = trim(x);
    let x = &x[a..b];
    let dur = x.len() as f64 / SR as f64;
    let info = describe(&track_f0_default(x, SR), dur);
    let hop = (0.005 * SR as f64) as usize;
    let n = (x.len() / hop).max(1);
    let env: Vec<f64> = if x.len() >= hop {
        (0..n)
            .map(|k| {
                let s: f64 = x[k * hop..(k + 1) * hop].iter().map(|v| (*v as f64).powi(2)).sum();
                (s / hop as f64).sqrt()
            })
            .collect()
    } else {
        vec![0.0]
    };
    let peak = env.iter().cloned().fold(0.0f64, f64::max);
    let attack = if peak > 0.0 { env.iter().position(|e| *e >= 0.8 * peak).unwrap_or(0) as f64 * 0.005 } else { 1.0 };
    let head_len = ((0.06 * SR as f64) as usize).min(x.len());
    let centroid = if head_len >= 256 {
        let w = np::hanning(head_len);
        let sig: Vec<f64> = x[..head_len].iter().zip(&w).map(|(v, h)| *v as f64 * h).collect();
        let spec: Vec<f64> = np::rfft(&sig, head_len).iter().map(|c| c.norm()).collect();
        let freqs = np::rfftfreq(head_len, 1.0 / SR as f64);
        let num: f64 = spec.iter().zip(&freqs).map(|(s, f)| s * f).sum();
        num / spec.iter().sum::<f64>().max(1e-9)
    } else {
        0.0
    };
    let (hf, lf) = if x.len() >= 256 {
        let w = np::hanning(x.len());
        let sig: Vec<f64> = x.iter().zip(&w).map(|(v, h)| *v as f64 * h).collect();
        let power: Vec<f64> = np::rfft(&sig, x.len()).iter().map(|c| c.norm_sqr()).collect();
        let freqs = np::rfftfreq(x.len(), 1.0 / SR as f64);
        let total = power.iter().sum::<f64>().max(1e-12);
        let hf: f64 = power.iter().zip(&freqs).filter(|(_, f)| **f >= 4000.0).map(|(p, _)| *p).sum::<f64>() / total;
        let lf: f64 = power.iter().zip(&freqs).filter(|(_, f)| **f < 250.0).map(|(p, _)| *p).sum::<f64>() / total;
        (hf, lf)
    } else {
        (0.0, 0.0)
    };
    let rms = if x.is_empty() { 0.0 } else { (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt() };
    let crest = if rms > 0.0 { peak / rms } else { 0.0 };
    Measure {
        dur: round_to(dur, 4),
        f0: info.f0.map(|f| round_to(f, 2)),
        midi: info.midi.map(|m| round_to(m, 2)),
        voiced: round_to(info.voiced_ratio, 3),
        stability: round_to(info.stability.min(1200.0), 1),
        attack: round_to(attack, 4),
        centroid: round_to(centroid, 1),
        hf: round_to(hf, 3),
        lf: round_to(lf, 3),
        crest: round_to(crest, 2),
    }
}

/// 1 inside `[lo, hi]`, falling to 0 over `soft` either side.
fn band(v: f64, lo: f64, hi: f64, soft: f64) -> f64 {
    if lo <= v && v <= hi {
        return 1.0;
    }
    let d = if v < lo { lo - v } else { v - hi };
    (1.0 - d / soft).max(0.0)
}

/// How well a sound suits a role. For drums, `m` measures the cut hit.
pub fn score(role: &str, m: &Measure) -> f64 {
    let sharp = band(m.attack, 0.0, 0.015, 0.05);
    // Shaped hits land in a narrow range, so the band is narrow too.
    let punch = band(m.crest, 2.4, 8.0, 0.8);
    match role {
        // A thump is *low*, and a low centroid is not the same thing: what
        // makes a kick is energy under 250 Hz, arriving all at once.
        "drums:kick" => 2.5 * band(m.lf, 0.45, 1.0, 0.35) + 2.0 * sharp + punch + 0.5 * band(m.centroid, 0.0, 400.0, 500.0),
        // A crack then a hiss: sharp, noisy rather than sung, and it punches.
        "drums:snare" => 2.0 * sharp + 2.0 * band(m.hf, 0.35, 1.0, 0.3) + (1.0 - m.voiced) + punch,
        "drums:hats" => 3.0 * m.hf + band(m.centroid, 6000.0, 16000.0, 4000.0) + 0.5 * (1.0 - m.voiced) + 0.5 * punch,
        // A tom is a kick with a note in it.
        "drums:toms" => 1.5 * band(m.lf, 0.25, 0.9, 0.3) + 1.5 * sharp + m.voiced + 0.5 * punch + band(m.centroid, 0.0, 1200.0, 1000.0),
        // A cymbal is a hiss that rings: bright, and long enough to hear it go.
        "drums:cymbals" => 3.0 * m.hf + 1.5 * band(m.dur, 0.25, 0.6, 0.3) + 0.5 * (1.0 - m.voiced),
        r if r.starts_with("drums") => 2.5 * sharp + (1.0 - m.voiced) + punch,
        // Pitched: the whole point is a note, so no pitch at all is disqualifying.
        _ => {
            let Some(f0) = m.f0.filter(|f| *f != 0.0) else { return -5.0 };
            let mut s = 2.5 * m.voiced + 1.5 * (1.0 - m.stability / 150.0).max(0.0) + band(m.dur, 0.25, 0.8, 0.3);
            if role == "bass" {
                s += 0.5 * band(f0, 60.0, 150.0, 80.0);
            }
            s
        }
    }
}

/// The suggested sound for one part.
#[derive(Clone, Debug)]
pub struct Rec {
    pub text: Option<String>,
    pub take: usize,
    pub takes: Vec<usize>,
    pub pool: Vec<(String, usize)>,
    pub octave: i32,
    pub mode: String,
    pub score: Option<f64>,
    pub pitch: Option<Measure>,
    pub alternatives: Vec<(String, usize, f64)>,
}

impl Rec {
    pub fn empty(part: &Part) -> Rec {
        Rec {
            text: None,
            take: 0,
            takes: Vec::new(),
            pool: Vec::new(),
            octave: 0,
            mode: if part.is_drums() { "raw" } else { "perfect" }.to_string(),
            score: None,
            pitch: None,
            alternatives: Vec::new(),
        }
    }

    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("text".into(), json!(self.text));
        m.insert("take".into(), json!(self.take));
        if self.text.is_some() {
            m.insert("takes".into(), json!(self.takes));
            m.insert(
                "pool".into(),
                Value::Array(self.pool.iter().map(|(t, k)| json!({ "text": t, "take": k })).collect()),
            );
        }
        m.insert("octave".into(), json!(self.octave));
        m.insert("mode".into(), json!(self.mode));
        m.insert("score".into(), json!(self.score));
        m.insert("pitch".into(), self.pitch.as_ref().map_or(Value::Null, Measure::to_json));
        m.insert(
            "alternatives".into(),
            Value::Array(self.alternatives.iter().map(|(t, k, s)| json!({ "text": t, "take": k, "score": s })).collect()),
        );
        Value::Object(m)
    }
}

/// Measurements kept on disk per corpus.
pub struct AnalysisStore {
    dir: PathBuf,
    mem: Mutex<HashMap<String, Arc<Mutex<HashMap<String, Value>>>>>,
}

impl AnalysisStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into(), mem: Mutex::new(HashMap::new()) }
    }

    fn path(&self, corpus: &str) -> PathBuf {
        self.dir.join(format!("analysis-{corpus}.json"))
    }

    fn load(&self, corpus: &str) -> Arc<Mutex<HashMap<String, Value>>> {
        let mut g = self.mem.lock().unwrap();
        g.entry(corpus.to_string())
            .or_insert_with(|| {
                let data = std::fs::read(self.path(corpus))
                    .ok()
                    .and_then(|b| serde_json::from_slice::<HashMap<String, Value>>(&b).ok())
                    .unwrap_or_default();
                Arc::new(Mutex::new(data))
            })
            .clone()
    }

    fn save(&self, corpus: &str, data: &HashMap<String, Value>) {
        let _ = std::fs::create_dir_all(&self.dir);
        let tmp = self.dir.join(format!("analysis-{corpus}.{:x}.tmp", std::process::id() as u64 ^ rand_bits()));
        if let Ok(bytes) = serde_json::to_vec(data) {
            if std::fs::write(&tmp, bytes).is_ok() {
                let _ = std::fs::rename(&tmp, self.path(corpus));
            }
        }
        let _ = std::fs::remove_file(&tmp);
    }

    /// Forget what was measured for a corpus (it was replaced).
    pub fn forget(&self, corpus: &str) {
        self.mem.lock().unwrap().remove(corpus);
        let _ = std::fs::remove_file(self.path(corpus));
    }
}

fn rand_bits() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new().build_hasher().finish()
}

fn words_for(role: &str, bank: &Bank, frequent: &[String], noises: &[String], units: &std::collections::BTreeMap<String, Vec<Take>>) -> Vec<String> {
    let mut words: Vec<String> =
        preferred(role).iter().filter(|w| bank.clip_count(w) > 0).map(|w| w.to_string()).collect();
    if !units.is_empty() {
        let mut head: Vec<String> = unit_words(role)
            .iter()
            .filter(|u| units.get(u.trim_matches('/')).is_some_and(|v| v.len() >= 3))
            .map(|u| u.to_string())
            .collect();
        head.extend(words);
        words = head;
    }
    // A corpus with non-verbal noises has better drums in it than any word: a
    // spew or a click is already a percussive sound, so they are auditioned
    // for the kit alongside the words and win on their own merits.
    if role.starts_with("drums") {
        let mut head: Vec<String> = noises.iter().map(|k| format!("*{k}*")).collect();
        head.extend(words);
        words = head;
    }
    if words.len() < 3 {
        let have: HashSet<String> = words.iter().cloned().collect();
        words.extend(frequent.iter().filter(|w| !have.contains(*w)).cloned());
    }
    words
}

/// The clip measured whole, and cut into each drum hit in `groups`.
fn measure_clip(env: &Env, take: &Take, groups: &[String], keep_going: &dyn Fn() -> bool) -> Option<Value> {
    let audio = AudioCache::new(env.renderer.audio());
    let seg = take.segment();
    let (start, end) = extract_window(&seg, PAD_END, &audio);
    let x = decode_audio(env.ffmpeg, &seg.source_file, Some(start), Some(end - start), keep_going).ok()?;
    if !keep_going() {
        return None;
    }
    let mut out = Map::new();
    out.insert("v".into(), json!(MEASURE_VERSION));
    out.insert("word".into(), measure(&x).to_json());
    let (a, b) = trim(&x);
    for g in groups {
        out.insert(g.clone(), measure(&drums::shape(&x[a..b], g).0).to_json());
    }
    Some(Value::Object(out))
}

/// For each part: the suggested sound, alternatives, and an octave to play it in.
pub fn recommend(
    song: &Song,
    bank: &Bank,
    env: &Env,
    store: &AnalysisStore,
    progress: Progress,
) -> Result<Vec<(String, Rec)>> {
    let corpus = bank.name.clone();
    // The most frequent whole words, as a fallback.
    let mut counted: Vec<(&String, usize)> =
        bank.pool.by_word.iter().filter(|(w, _)| !w.is_empty() && w.chars().all(char::is_alphabetic)).map(|(w, v)| (w, v.len())).collect();
    counted.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    let frequent: Vec<String> = counted.into_iter().take(FALLBACK_WORDS).map(|(w, _)| w.clone()).collect();

    let mut roles: Vec<String> = song.parts.iter().map(|p| p.role.clone()).collect();
    roles.sort();
    roles.dedup();
    // "noise" is the any-of pool: every take is a different kind of sound, so
    // it is no use as one instrument. It stays available to type by hand.
    let noise_kinds: Vec<String> = bank.noises().into_iter().map(|(k, _)| k).filter(|k| k != "noise").collect();
    let units = bank.units();

    // Every (word, take) worth measuring for any role, measured once.
    let mut wanted: OrderedMap<Vec<(usize, Take)>> = OrderedMap::new();
    for role in &roles {
        for w in words_for(role, bank, &frequent, &noise_kinds, units) {
            if wanted.contains_key(&w) {
                continue;
            }
            let tk = bank.takes_for(&w);
            if tk.is_empty() {
                continue;
            }
            let step = (tk.len() / TAKES_PER_WORD).max(1);
            let picks: Vec<(usize, Take)> = (0..tk.len()).step_by(step).take(TAKES_PER_WORD).map(|i| (i, tk[i].clone())).collect();
            wanted.insert(w, picks);
        }
    }

    let cache = store.load(&corpus);
    let mut groups: Vec<String> =
        roles.iter().filter_map(|r| r.strip_prefix("drums:")).map(str::to_string).collect();
    groups.sort();
    groups.dedup();

    let needs = |m: Option<&Value>| -> bool {
        match m {
            Some(m) => {
                m.get("v").and_then(Value::as_i64) != Some(MEASURE_VERSION)
                    || m.get("word").is_none()
                    || groups.iter().any(|g| m.get(g).is_none())
            }
            None => true,
        }
    };
    let todo: Vec<Take> = {
        let c = cache.lock().unwrap();
        let mut seen = HashSet::new();
        let mut todo = Vec::new();
        for (_, pairs) in wanted.iter() {
            for (_, t) in pairs {
                let k = t.cache_key();
                if needs(c.get(&k)) && seen.insert(k) {
                    todo.push(t.clone());
                }
            }
        }
        todo
    };
    if !todo.is_empty() {
        tracing::info!("ytpmv: measuring {} takes for {corpus}", todo.len());
        say(progress, "measuring", 0, todo.len())?;
        let results: Mutex<Vec<Option<Value>>> = Mutex::new(vec![None; todo.len()]);
        let next = AtomicUsize::new(0);
        let done = AtomicUsize::new(0);
        let stopped = AtomicBool::new(false);
        let workers = std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(1, 4);
        std::thread::scope(|s| {
            for _ in 0..workers {
                s.spawn(|| loop {
                    if stopped.load(Ordering::Relaxed) {
                        return;
                    }
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= todo.len() {
                        return;
                    }
                    let keep = || !stopped.load(Ordering::Relaxed);
                    let m = measure_clip(env, &todo[i], &groups, &keep);
                    results.lock().unwrap()[i] = m;
                    let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                    if !progress("measuring", d, todo.len()) {
                        stopped.store(true, Ordering::Relaxed);
                    }
                });
            }
        });
        if stopped.load(Ordering::Relaxed) {
            return Err(crate::Error::Cancelled);
        }
        let results = results.into_inner().unwrap();
        let mut c = cache.lock().unwrap();
        for (t, m) in todo.iter().zip(results) {
            let Some(Value::Object(new)) = m else { continue };
            let key = t.cache_key();
            let mut merged = match c.get(&key) {
                Some(Value::Object(old)) if old.get("v").and_then(Value::as_i64) == Some(MEASURE_VERSION) => old.clone(),
                _ => Map::new(),
            };
            merged.extend(new);
            c.insert(key, Value::Object(merged));
        }
        store.save(&corpus, &c);
    }

    let mut heard: HashMap<(String, usize), bool> = HashMap::new();
    let mut sings = |word: &str, take: usize| -> bool {
        *heard.entry((word.to_string(), take)).or_insert_with(|| {
            bank.build(env, word, take, None, &|| progress("samples", 0, song.parts.len()))
                .map(|s| s.voice.info.f0.is_some())
                .unwrap_or(false)
        })
    };

    let mut out: Vec<(String, Rec)> = Vec::new();
    let mut used: HashMap<String, i32> = HashMap::new();
    let c = cache.lock().unwrap().clone();
    for part in &song.parts {
        // (score, word, take, measure)
        let mut ranked: Vec<(f64, String, usize, Measure)> = Vec::new();
        for w in words_for(&part.role, bank, &frequent, &noise_kinds, units) {
            for (i, take) in wanted.get(&w).into_iter().flatten() {
                let Some(entry) = c.get(&take.cache_key()) else { continue };
                let key = if part.is_drums() { part.drum_group.as_deref().unwrap_or("perc") } else { "word" };
                let Some(m) = entry.get(key).and_then(Measure::from_json) else { continue };
                // Nothing gets a head start — not the words at the top of the
                // list, and not the noises. The measurements decide. The one
                // thumb on the scale is against repetition: the same sound on
                // every tile is a worse video than a slightly less ideal
                // second choice.
                let penalty = 0.6 * *used.get(&w).unwrap_or(&0) as f64;
                ranked.push((score(&part.role, &m) - penalty, w.clone(), *i, m));
            }
        }
        ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        // One entry per word in the alternatives, best take of each.
        let mut seen = HashSet::new();
        let mut uniq: Vec<(f64, String, usize, Measure)> = Vec::new();
        for r in &ranked {
            if seen.insert(r.1.clone()) {
                uniq.push(r.clone());
            }
        }
        if !part.is_drums() {
            // The measurement is taken from the recording; what is played is
            // the sample built from it, and the two can disagree. A pitched
            // part that cannot be put on a note is worse than the next
            // suggestion down, so the top few are built and the first that
            // really has a pitch is the one used.
            let pick = uniq.iter().take(SING_TRIES).position(|r| r.3.f0.is_some() && sings(&r.1, r.2));
            if let Some(k) = pick {
                let r = uniq.remove(k);
                uniq.insert(0, r);
            }
        }
        if uniq.is_empty() {
            out.push((part.id.clone(), Rec::empty(part)));
            continue;
        }
        let best = uniq[0].clone();
        *used.entry(best.1.clone()).or_insert(0) += 1;
        let m = best.3.clone();
        // The best few takes of the word it chose, best first. Playing one
        // recording on every beat is what makes a part sound like a loop
        // rather than a drummer; the renderer rotates through these.
        let rotate: Vec<usize> = ranked.iter().filter(|r| r.1 == best.1).map(|r| r.2).take(ROTATE).collect();
        // For the ultra mode: the words that would also have done, so it can
        // pick a different one on every hit.
        let pool: Vec<(String, usize)> = uniq
            .iter()
            .take(POOL)
            .filter(|r| r.0 > 0.0 && (part.is_drums() || r.3.f0.is_some()))
            .map(|r| (r.1.clone(), r.2))
            .collect();
        out.push((
            part.id.clone(),
            Rec {
                text: Some(best.1.clone()),
                take: best.2,
                takes: if rotate.is_empty() { vec![best.2] } else { rotate },
                pool: if pool.is_empty() { vec![(best.1.clone(), best.2)] } else { pool },
                octave: if part.is_drums() { 0 } else { auto_octave(part.median_pitch(), m.midi) },
                // A voice with no pitch (a whisper) cannot be put *on* a note,
                // but tape speed still follows the tune up and down.
                mode: if part.is_drums() { "raw" } else if m.f0.is_some() { "perfect" } else { "tape" }.to_string(),
                score: Some(round_to(best.0, 2)),
                pitch: Some(m),
                alternatives: uniq
                    .iter()
                    .skip(1)
                    .take(ALTERNATIVES)
                    .map(|r| (r.1.clone(), r.2, round_to(r.0, 2)))
                    .collect(),
            },
        ));
    }
    Ok(out)
}
