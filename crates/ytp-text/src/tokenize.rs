//! Tokeniser: text -> the words to say, with everything the markup says about
//! each (`~reverse~`, `*noise*`, `word^+2`, `word{pause=0.5}`, held letters).
//! Ported from `app/generate.py` (`tokenize_full`, `tokenize_marked`,
//! `_expand_token`, `unstretch`, `parse_fx`).

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{json, Value};

use crate::{clen, cmudict, is_word_char, num2words, phonemes};

/// What the tokeniser needs to know about the corpus.
pub trait Lexicon {
    /// How many recordings the corpus has of exactly this word.
    fn clip_count(&self, word: &str) -> usize;

    fn known(&self, word: &str) -> bool {
        !word.is_empty() && self.clip_count(word) > 0
    }
}

/// No corpus: expansion rules only.
pub struct NoLexicon;

impl Lexicon for NoLexicon {
    fn clip_count(&self, _word: &str) -> usize {
        0
    }
}

impl<S: std::hash::BuildHasher> Lexicon for HashMap<String, usize, S> {
    fn clip_count(&self, word: &str) -> usize {
        self.get(word).copied().unwrap_or(0)
    }
}

pub const GLITCH_NAMES: [&str; 13] = [
    "shake", "punch", "spin", "rainbow", "invert", "flip", "strobe", "crunch", "fringe",
    "earrape", "wobble", "cave", "stutter",
];

const UNITS: [(&str, &str); 11] = [
    ("km", "kilometres"),
    ("kmh", "kilometres per hour"),
    ("kph", "kilometres per hour"),
    ("mph", "miles per hour"),
    ("kg", "kilograms"),
    ("hp", "horsepower"),
    ("kw", "kilowatts"),
    ("nm", "newton metres"),
    ("mm", "millimetres"),
    ("cm", "centimetres"),
    ("ml", "millilitres"),
];
const UNITS_AFTER_NUMBER: [(&str, &str); 5] = [
    ("l", "litres"),
    ("m", "metres"),
    ("g", "grams"),
    ("k", "thousand"),
    ("t", "tonnes"),
];
const DECADES: [(&str, &str); 8] = [
    ("20", "twenties"),
    ("30", "thirties"),
    ("40", "forties"),
    ("50", "fifties"),
    ("60", "sixties"),
    ("70", "seventies"),
    ("80", "eighties"),
    ("90", "nineties"),
];
const PER_UNITS: [(&str, &str); 10] = [
    ("h", "hour"),
    ("hr", "hour"),
    ("s", "second"),
    ("sec", "second"),
    ("min", "minute"),
    ("km", "kilometre"),
    ("100km", "hundred kilometres"),
    ("l", "litre"),
    ("kg", "kilogram"),
    ("m", "metre"),
];
const CURRENCY: [(char, &str); 3] = [('$', "dollars"), ('£', "pounds"), ('€', "euros")];
const SPELLING_SWAPS: [(&str, &str); 6] = [
    ("res", "ers"),
    ("re", "er"),
    ("our", "or"),
    ("ise", "ize"),
    ("ised", "ized"),
    ("yse", "yze"),
];

fn lookup<'a>(table: &'a [(&str, &str)], key: &str) -> Option<&'a str> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("regex"))
}

macro_rules! lazy_re {
    ($pat:expr) => {{
        static CELL: OnceLock<Regex> = OnceLock::new();
        re(&CELL, $pat)
    }};
}

/// A word as the corpus stores it: lower case, letters and digits only
/// (`apostrophe` keeps `'`).
pub fn clean_word(w: &str, apostrophe: bool) -> String {
    w.chars()
        .filter(|&c| is_word_char(c) || (apostrophe && c == '\''))
        .collect::<String>()
        .to_lowercase()
}

fn split_words(s: &str) -> Vec<String> {
    s.replace(',', "")
        .split(|c: char| c.is_whitespace() || c == '-')
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

fn known_spelling(word: &str) -> String {
    if phonemes::word_to_phonemes(word).is_some() {
        return word.to_string();
    }
    for (british, american) in SPELLING_SWAPS {
        if let Some(stem) = word.strip_suffix(british) {
            let alt = format!("{stem}{american}");
            if phonemes::word_to_phonemes(&alt).is_some() {
                return alt;
            }
        }
    }
    word.to_string()
}

fn unit_words(phrase: &str) -> Vec<String> {
    split_words(phrase).iter().map(|w| known_spelling(w)).collect()
}

fn say_number(digits: &str, ordinal: bool) -> Vec<String> {
    match digits.parse::<u128>() {
        Ok(n) => split_words(&if ordinal {
            num2words::ordinal(n)
        } else {
            num2words::cardinal(n)
        }),
        Err(_) => digits.chars().flat_map(|c| say_number(&c.to_string(), false)).collect(),
    }
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

const STRIP: &[char] = &['.', ',', '!', '?', ';', ':', '\'', '"', '(', ')', '[', ']'];

/// One separator-free token into the words a person would say for it.
fn expand_atom(atom: &str) -> Vec<String> {
    if atom.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut atom = atom;
    let mut suffix: Vec<String> = Vec::new();
    let first = atom.chars().next().unwrap();
    if let Some((_, name)) = CURRENCY.iter().find(|(c, _)| *c == first) {
        suffix.push(name.to_string());
        atom = &atom[first.len_utf8()..];
    }
    if let Some(rest) = atom.strip_suffix('%') {
        suffix.push("percent".into());
        atom = rest;
    }
    let mut atom: String = atom.trim_matches(STRIP).to_lowercase();
    if atom.is_empty() {
        return suffix;
    }

    if let Some(m) = lazy_re!(r"\A([a-z]+)'(s|re|ve|ll|d|t|m)\z").captures(&atom) {
        atom = format!("{}{}", &m[1], &m[2]);
    }
    if lazy_re!(r"\A[0-9]{1,3}(?:,[0-9]{3})+\z").is_match(&atom) {
        atom = atom.replace(',', "");
    }

    if let Some(m) = lazy_re!(r"\A([0-9]+)\.([0-9]+)([a-z]+)?\z").captures(&atom) {
        out.extend(say_number(&m[1], false));
        out.push("point".into());
        for d in m[2].chars() {
            out.extend(say_number(&d.to_string(), false));
        }
        if let Some(unit) = m.get(3) {
            let unit = unit.as_str();
            let phrase = lookup(&UNITS, unit)
                .or_else(|| lookup(&UNITS_AFTER_NUMBER, unit))
                .unwrap_or(unit);
            out.extend(unit_words(phrase));
        }
        out.extend(suffix);
        return out;
    }

    if let Some(m) = lazy_re!(r"\A([0-9]+)(?:st|nd|rd|th)\z").captures(&atom) {
        let mut v = say_number(&m[1], true);
        v.extend(suffix);
        return v;
    }

    if let Some(m) = lazy_re!(r"\A(?:(19|20))?([0-9]0)s\z").captures(&atom) {
        if let Some(name) = lookup(&DECADES, &m[2]) {
            let mut v = match m.get(1) {
                Some(c) => say_number(c.as_str(), false),
                None => Vec::new(),
            };
            v.push(name.to_string());
            v.extend(suffix);
            return v;
        }
    }

    if let Some(m) = lazy_re!(r"\A([0-9]+)x([0-9]+)\z").captures(&atom) {
        let mut v = say_number(&m[1], false);
        v.push("by".into());
        v.extend(say_number(&m[2], false));
        v.extend(suffix);
        return v;
    }

    if is_digits(&atom) {
        let mut v = say_number(&atom, false);
        v.extend(suffix);
        return v;
    }

    if let Some(u) = lookup(&UNITS, &atom) {
        let mut v = unit_words(u);
        v.extend(suffix);
        return v;
    }

    if atom == "mk" {
        let mut v = vec!["mark".to_string()];
        v.extend(suffix);
        return v;
    }

    let parts: Vec<&str> = lazy_re!(r"[0-9]+|[a-z]+")
        .find_iter(&atom)
        .map(|m| m.as_str())
        .collect();
    if parts.len() > 1 {
        let mut after_number = false;
        for part in parts {
            if is_digits(part) {
                out.extend(say_number(part, false));
                after_number = true;
                continue;
            }
            if let Some(u) = lookup(&UNITS, part) {
                out.extend(unit_words(u));
            } else if let (true, Some(u)) = (after_number, lookup(&UNITS_AFTER_NUMBER, part)) {
                out.extend(unit_words(u));
            } else if part == "mk" {
                out.push("mark".into());
            } else {
                out.push(part.to_string());
            }
            after_number = false;
        }
        out.extend(suffix);
        return out;
    }

    let stripped: String = atom.chars().filter(|&c| is_word_char(c)).collect();
    if stripped.is_empty() {
        suffix
    } else {
        let mut v = vec![stripped];
        v.extend(suffix);
        v
    }
}

/// One or more lowercase words for a single input token. What the corpus
/// actually says wins over the rules.
pub fn expand_token(token: &str, lex: &dyn Lexicon) -> Vec<String> {
    let raw = clean_word(token, false);
    let has_digit = token.chars().any(|c| c.is_ascii_digit());
    let numeric_punctuation = has_digit
        && (lazy_re!(r"[0-9]\.[0-9]").is_match(token)
            || token.chars().any(|c| CURRENCY.iter().any(|(s, _)| *s == c) || c == '%'));
    if !numeric_punctuation && lex.known(&raw) {
        return vec![raw];
    }
    let trimmed = token.trim_matches(&['.', ',', '!', '?', ';', ':'][..]).to_lowercase();
    if let Some(m) = lazy_re!(r"\A([a-z]+)/([0-9]*[a-z]+)\z").captures(&trimmed) {
        if let Some(den) = lookup(&PER_UNITS, &m[2]) {
            let top = lookup(&UNITS, &m[1]).or_else(|| lookup(&UNITS_AFTER_NUMBER, &m[1]));
            if let Some(top) = top {
                let mut v = unit_words(top);
                v.push("per".into());
                v.extend(unit_words(den));
                return v;
            }
        }
    }
    let mut out = Vec::new();
    for atom in lazy_re!(r"[/\-–—]+").split(token) {
        out.extend(expand_atom(atom));
    }
    out
}

/// The plain word list for `text` (no markup).
pub fn tokenize(text: &str, lex: &dyn Lexicon) -> Vec<String> {
    text.split_whitespace()
        .flat_map(|t| expand_token(t, lex))
        .collect()
}

const UNHOLDABLE: &str = "bcdgkpqt";

/// Where a stretched spelling is held: (position, letters, extra letters).
pub type StretchMark = (usize, usize, usize);

/// Runs of one repeated letter in `raw`: (start, length), length >= 2.
fn letter_runs(raw: &[char]) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let mut j = i;
        while j < raw.len() && raw[j] == raw[i] {
            j += 1;
        }
        if raw[i].is_ascii_lowercase() && j - i >= 2 {
            runs.push((i, j - i));
        }
        i = j;
    }
    runs
}

/// The word a stretched spelling means and where it is stretched, or `None`.
pub fn unstretch(token: &str, lex: &dyn Lexicon) -> Option<(String, Vec<StretchMark>)> {
    let m = lazy_re!(r#"\A([A-Za-z]+)([.,!?;:'"()\]]*)\z"#).captures(token)?;
    let raw_s = m[1].to_lowercase();
    let raw: Vec<char> = raw_s.chars().collect();
    let known = lex.clip_count(&raw_s) > 0 || cmudict::contains(&raw_s);
    let runs: Vec<(usize, usize)> = letter_runs(&raw)
        .into_iter()
        .filter(|&(_, n)| n >= 3 || (n == 2 && !known))
        .collect();
    if runs.is_empty() || runs.len() > 6 {
        return None;
    }
    let mut best: Option<(String, Vec<StretchMark>)> = None;
    let mut best_rank: Option<(usize, bool, i64)> = None;
    let combos = 1usize << runs.len();
    for bits in 0..combos {
        // itertools.product order: the first run varies slowest.
        let keeps: Vec<usize> = (0..runs.len())
            .map(|k| 1 + ((bits >> (runs.len() - 1 - k)) & 1))
            .collect();
        let mut word: Vec<String> = Vec::new();
        let mut marks: Vec<StretchMark> = Vec::new();
        let mut at = 0usize;
        for (&(p, n), &keep) in runs.iter().zip(&keeps) {
            word.push(raw[at..p].iter().collect());
            let pos: usize = word.iter().map(|x| x.chars().count()).sum();
            word.push(std::iter::repeat_n(raw[p], keep).collect());
            marks.push((pos, keep, n - keep));
            at = p + n;
        }
        word.push(raw[at..].iter().collect());
        let w: String = word.concat();
        let rank = (lex.clip_count(&w), cmudict::contains(&w), -(clen(&w) as i64));
        if best_rank.is_none_or(|b| rank > b) {
            best_rank = Some(rank);
            best = Some((w, marks));
        }
    }
    let (w, marks) = best?;
    let wc: Vec<char> = w.chars().collect();
    let marks = marks
        .into_iter()
        .filter(|&(p, _, e)| e > 0 && !UNHOLDABLE.contains(wc[p]))
        .collect();
    Some((w, marks))
}

/// Where a word sits when sung: `("rel", semitones)` or `("abs", midi)`.
#[derive(Debug, Clone, PartialEq)]
pub enum Note {
    Rel(f64),
    Abs(f64),
}

impl Note {
    pub fn to_json(&self) -> Value {
        match self {
            Note::Rel(v) => json!(["rel", v]),
            Note::Abs(v) => json!(["abs", v]),
        }
    }
}

pub fn parse_mark(mark: &str) -> Option<Note> {
    if mark.is_empty() {
        return None;
    }
    if let Some(m) = lazy_re!(r"\A([A-Ga-g])([#b]?)(-?[0-9])\z").captures(mark) {
        let pc = match m[1].to_lowercase().as_str() {
            "c" => 0,
            "d" => 2,
            "e" => 4,
            "f" => 5,
            "g" => 7,
            "a" => 9,
            _ => 11,
        } + match &m[2] {
            "#" => 1,
            "b" => -1,
            _ => 0,
        };
        let octave: i32 = m[3].parse().ok()?;
        return Some(Note::Abs(((octave + 1) * 12 + pc) as f64));
    }
    mark.parse::<i32>().ok().map(|v| Note::Rel(v as f64))
}

/// Per-word effects as typed in `hello{pause=0.5,rate=0.8,...}`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Fx {
    pub spell: bool,
    pub glitch: Vec<String>,
    pub boom: bool,
    pub vocode: bool,
    pub voice: Option<String>,
    pub clip: Option<i64>,
    pub emph: Option<String>,
    pub pause: Option<f64>,
    pub rate: Option<f64>,
    pub pitch: Option<f64>,
    pub vol: Option<f64>,
}

impl Fx {
    pub fn is_empty(&self) -> bool {
        *self == Fx::default()
    }

    pub fn to_json(&self) -> Value {
        let mut m = serde_json::Map::new();
        if self.spell {
            m.insert("spell".into(), json!(true));
        }
        if !self.glitch.is_empty() {
            m.insert("glitch".into(), json!(self.glitch));
        }
        if self.boom {
            m.insert("boom".into(), json!(true));
        }
        if self.vocode {
            m.insert("vocode".into(), json!(true));
        }
        if let Some(v) = &self.voice {
            m.insert("voice".into(), json!(v));
        }
        if let Some(v) = self.clip {
            m.insert("clip".into(), json!(v));
        }
        if let Some(v) = &self.emph {
            m.insert("emph".into(), json!(v));
        }
        for (k, v) in [
            ("pause", self.pause),
            ("rate", self.rate),
            ("pitch", self.pitch),
            ("vol", self.vol),
        ] {
            if let Some(v) = v {
                m.insert(k.into(), json!(v));
            }
        }
        Value::Object(m)
    }
}

fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    if v.is_nan() {
        lo
    } else {
        v.max(lo).min(hi)
    }
}

pub fn parse_fx(body: &str) -> Fx {
    let mut fx = Fx::default();
    for item in body.split(',') {
        let item = item.trim();
        let (key, val) = item.split_once('=').unwrap_or((item, ""));
        let key = key.trim().to_lowercase();
        let key = key.as_str();
        if key == "spell" {
            fx.spell = true;
        } else if GLITCH_NAMES.contains(&key) && val.is_empty() {
            if !fx.glitch.iter().any(|g| g == key) {
                fx.glitch.push(key.to_string());
            }
        } else if key == "boom" && val.is_empty() {
            fx.boom = true;
        } else if key == "vocode" && val.is_empty() {
            fx.vocode = true;
        } else if (key == "v" || key == "voice") && !val.trim().is_empty() {
            let v: String = val
                .trim()
                .chars()
                .filter(|&c| is_word_char(c) || c == '-')
                .take(40)
                .collect();
            fx.voice = Some(v);
        } else if key == "clip" && is_digits(val.trim()) {
            fx.clip = val.trim().parse().ok();
        } else if key == "emph" && ["reduced", "moderate", "strong"].contains(&val.trim()) {
            fx.emph = Some(val.trim().to_string());
        } else if let Some((lo, hi)) = match key {
            "pause" => Some((0.0, 3.0)),
            "rate" => Some((0.5, 2.0)),
            "pitch" | "vol" => Some((-12.0, 12.0)),
            _ => None,
        } {
            if let Ok(v) = val.trim().parse::<f64>() {
                let v = clamp(v, lo, hi);
                match key {
                    "pause" => fx.pause = Some(v),
                    "rate" => fx.rate = Some(v),
                    "pitch" => fx.pitch = Some(v),
                    _ => fx.vol = Some(v),
                }
            }
        }
    }
    fx
}

/// rate, pitch, vol and pause with emphasis folded in.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EffectiveFx {
    pub rate: Option<f64>,
    pub pitch: Option<f64>,
    pub vol: Option<f64>,
    pub pause: Option<f64>,
    pub clip: Option<i64>,
    pub glitch: Vec<String>,
    pub boom: bool,
    pub vocode: bool,
    pub voice: Option<String>,
}

impl EffectiveFx {
    pub fn is_empty(&self) -> bool {
        *self == EffectiveFx::default()
    }
}

pub fn effective_fx(fx: &Fx) -> EffectiveFx {
    let mut out = EffectiveFx::default();
    match fx.emph.as_deref() {
        Some("reduced") => {
            out.vol = Some(-4.0);
            out.rate = Some(1.1);
        }
        Some("moderate") => {
            out.vol = Some(3.0);
            out.rate = Some(0.92);
        }
        Some("strong") => {
            out.vol = Some(6.0);
            out.rate = Some(0.82);
            out.pitch = Some(1.0);
        }
        _ => {}
    }
    if let Some(r) = fx.rate {
        out.rate = Some(out.rate.unwrap_or(1.0) * r);
    }
    if let Some(p) = fx.pitch {
        out.pitch = Some(out.pitch.unwrap_or(0.0) + p);
    }
    if let Some(v) = fx.vol {
        out.vol = Some(out.vol.unwrap_or(0.0) + v);
    }
    out.pause = fx.pause;
    out.clip = fx.clip;
    out.glitch = fx.glitch.clone();
    out.boom = fx.boom;
    out.vocode = fx.vocode;
    out.voice = fx.voice.clone().filter(|v| !v.is_empty());
    out
}

fn letter_name(c: char) -> Option<&'static str> {
    Some(match c {
        'a' => "a",
        'b' => "bee",
        'c' => "see",
        'd' => "dee",
        'e' => "ee",
        'f' => "eff",
        'g' => "gee",
        'h' => "aitch",
        'i' => "i",
        'j' => "jay",
        'k' => "kay",
        'l' => "el",
        'm' => "em",
        'n' => "en",
        'o' => "oh",
        'p' => "pee",
        'q' => "queue",
        'r' => "are",
        's' => "es",
        't' => "tea",
        'u' => "you",
        'v' => "vee",
        'w' => "double you",
        'x' => "ex",
        'y' => "why",
        'z' => "zed",
        _ => return None,
    })
}

/// One word to say, with its markup.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub word: String,
    pub ends: bool,
    pub noise: bool,
    pub reverse: bool,
    pub stretch: Vec<StretchMark>,
    pub shown: String,
    pub note: Option<Note>,
    pub fx: Fx,
}

impl Token {
    pub fn to_json(&self) -> Value {
        json!({
            "word": self.word, "ends": self.ends, "noise": self.noise,
            "reverse": self.reverse,
            "stretch": self.stretch.iter().map(|s| json!([s.0, s.1, s.2])).collect::<Vec<_>>(),
            "shown": self.shown,
            "note": self.note.as_ref().map(Note::to_json),
            "fx": self.fx.to_json(),
        })
    }
}

fn ends_sentence(token: &str) -> bool {
    lazy_re!(r#"[.!?]["')\]]*\z"#).is_match(token)
}

/// Every word to say, with everything the markup says about it.
pub fn tokenize_full(text: &str, lex: &dyn Lexicon) -> Vec<Token> {
    let fx_re = lazy_re!(r#"\A(.*?)\{([\w=.,+\- ]*)\}([.,!?;:"')\]]*)\z"#);
    let mark_re = lazy_re!(r#"\A(.*?)\^([+-]?[0-9]{1,2}|[A-Ga-g][#b]?-?[0-9])([.,!?;:"')\]]*)\z"#);
    let rev_re = lazy_re!(r#"\A~(.+?)~([.!?"')\]]*)\z"#);
    let noise_re = lazy_re!(r"\A\*([A-Za-z]+)\*[.!?]*\z");
    let mut out: Vec<Token> = Vec::new();
    for token in text.split_whitespace() {
        let mut token = token.to_string();
        let mut fx = Fx::default();
        if let Some(m) = fx_re.captures(&token) {
            if !m[1].is_empty() {
                let (head, body, tail) = (m[1].to_string(), m[2].to_string(), m[3].to_string());
                token = format!("{head}{tail}");
                fx = parse_fx(&body);
            }
        }
        if std::mem::take(&mut fx.spell) {
            let letters: Vec<char> = token
                .to_lowercase()
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect();
            let ends = ends_sentence(&token);
            for (k, &c) in letters.iter().enumerate() {
                let said = match letter_name(c) {
                    Some(n) => n.to_string(),
                    None => expand_token(&c.to_string(), lex)
                        .into_iter()
                        .next()
                        .unwrap_or_else(|| c.to_string()),
                };
                let parts: Vec<&str> = said.split_whitespace().collect();
                for (j, w) in parts.iter().enumerate() {
                    out.push(Token {
                        word: w.to_string(),
                        ends: ends && k == letters.len() - 1 && j == parts.len() - 1,
                        noise: false,
                        reverse: false,
                        stretch: Vec::new(),
                        shown: c.to_uppercase().collect(),
                        note: None,
                        fx: fx.clone(),
                    });
                }
            }
            continue;
        }
        let mut note = None;
        if let Some(m) = mark_re.captures(&token) {
            if !m[1].is_empty() {
                let (head, mk, tail) = (m[1].to_string(), m[2].to_string(), m[3].to_string());
                token = format!("{head}{tail}");
                note = parse_mark(&mk);
            }
        }
        let mut rev = false;
        if let Some(m) = rev_re.captures(&token) {
            rev = true;
            token = format!("{}{}", &m[1], &m[2]);
        }
        let ends = ends_sentence(&token);
        if let Some(m) = noise_re.captures(&token) {
            let w = m[1].to_lowercase();
            out.push(Token {
                word: w.clone(),
                ends,
                noise: true,
                reverse: rev,
                stretch: Vec::new(),
                shown: w,
                note,
                fx,
            });
            continue;
        }
        let stretched = if lex.known(&clean_word(&token, false)) {
            None
        } else {
            unstretch(&token, lex)
        };
        if let Some((word, marks)) = stretched {
            out.push(Token {
                word,
                ends,
                noise: false,
                reverse: rev,
                stretch: marks,
                shown: clean_word(&token, true),
                note,
                fx,
            });
            continue;
        }
        let words = expand_token(&token, lex);
        let n = words.len();
        for (k, w) in words.into_iter().enumerate() {
            out.push(Token {
                ends: ends && k == n - 1,
                noise: false,
                reverse: rev,
                stretch: Vec::new(),
                shown: w.clone(),
                word: w,
                note: note.clone(),
                fx: fx.clone(),
            });
        }
    }
    if let Some(last) = out.last_mut() {
        last.ends = true;
    }
    out
}

/// The legacy `(word, ends_sentence, is_noise, is_reversed)` tuples.
pub fn tokenize_marked(text: &str, lex: &dyn Lexicon) -> Vec<(String, bool, bool, bool)> {
    tokenize_full(text, lex)
        .into_iter()
        .map(|t| (t.word, t.ends, t.noise, t.reverse))
        .collect()
}
