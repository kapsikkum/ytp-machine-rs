//! Word -> ARPAbet phonemes: overrides, CMUdict, inflection/derivation rules,
//! compound splitting and a spelling-based guess. Ported from `app/phonemes.py`
//! (the splice planner built on top of it lives in `ytp-splice`).
//!
//! The per-corpus `pronunciations.csv` of the Python app is not read.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use regex::Regex;

use crate::{clen, cmudict, cslice, num2words};

pub type Phones = Vec<String>;

const VOWELS: [&str; 15] = [
    "AA", "AE", "AH", "AO", "AW", "AY", "EH", "ER", "EY", "IH", "IY", "OW", "OY", "UH", "UW",
];

pub fn is_vowel(phone: &str) -> bool {
    VOWELS.contains(&phone)
}

/// Phoneme tables the Python module spells out in source.
pub struct Tables {
    pub overrides: HashMap<String, Phones>,
    pub spelled_out: HashSet<String>,
    pub said_as_word: HashMap<String, Phones>,
    pub letter_names: HashMap<char, Phones>,
    /// target word -> [(phoneme group, preferred source words)]
    pub recipes: HashMap<String, Vec<(Phones, Vec<String>)>>,
}

fn strings(v: &serde_json::Value) -> Vec<String> {
    v.as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

fn map_of(v: &serde_json::Value) -> HashMap<String, Phones> {
    v.as_object()
        .map(|o| o.iter().map(|(k, v)| (k.clone(), strings(v))).collect())
        .unwrap_or_default()
}

pub fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../data/tables.json")).expect("tables.json");
        Tables {
            overrides: map_of(&v["overrides"]),
            spelled_out: strings(&v["spelled_out"]).into_iter().collect(),
            said_as_word: map_of(&v["said_as_word"]),
            letter_names: map_of(&v["letter_names"])
                .into_iter()
                .filter_map(|(k, v)| k.chars().next().map(|c| (c, v)))
                .collect(),
            recipes: v["recipes"]
                .as_object()
                .map(|o| {
                    o.iter()
                        .map(|(k, groups)| {
                            let g = groups
                                .as_array()
                                .map(|a| a.iter().map(|p| (strings(&p[0]), strings(&p[1]))).collect())
                                .unwrap_or_default();
                            (k.clone(), g)
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    })
}

const CONTRACTION_ENDINGS: [&str; 6] = ["ve", "ll", "re", "d", "s", "m"];
const BRITISH_ENDINGS: [(&str, &str); 7] = [
    ("our", "or"),
    ("ise", "ize"),
    ("ised", "ized"),
    ("ising", "izing"),
    ("isation", "ization"),
    ("yse", "yze"),
    ("ogue", "og"),
];
const BRITISH_ANYWHERE: [(&str, &str); 9] = [
    ("our", "or"),
    ("ise", "ize"),
    ("isa", "iza"),
    ("yse", "yze"),
    ("ogue", "og"),
    ("aemi", "emi"),
    ("oeu", "eu"),
    ("tt", "t"),
    ("ll", "l"),
];

fn re_ending() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"([bcdfghjklmnpqrstvwxz])re(s?)$").unwrap())
}

fn dict(word: &str) -> Option<Phones> {
    cmudict::get(word)
}

fn base_phones(b: &str) -> Option<Phones> {
    dict(b)
}

fn letters_to_phones(letters: &str) -> Option<Phones> {
    let t = tables();
    let mut out = Vec::new();
    for ch in letters.chars() {
        out.extend(t.letter_names.get(&ch)?.iter().cloned());
    }
    (!out.is_empty()).then_some(out)
}

/// Phonemes for digits spoken as a cardinal or ordinal number.
fn number_to_phones(digits: &str, ordinal: bool) -> Option<Phones> {
    let n: u128 = digits.parse().ok()?;
    let spoken = if ordinal {
        num2words::ordinal(n)
    } else {
        num2words::cardinal(n)
    };
    let mut out = Vec::new();
    for part in spoken.split(|c: char| c.is_whitespace() || c == '-') {
        let part: String = part
            .to_lowercase()
            .chars()
            .filter(|c| c.is_ascii_lowercase())
            .collect();
        if part.is_empty() {
            continue;
        }
        out.extend(dict(&part)?);
    }
    (!out.is_empty()).then_some(out)
}

/// `re.findall(r"[a-z]+|\d+", w)`
fn alnum_parts(w: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut kind = 0; // 1 letters, 2 digits
    for c in w.chars() {
        let k = if c.is_ascii_lowercase() {
            1
        } else if c.is_ascii_digit() {
            2
        } else {
            0
        };
        if k != kind && !cur.is_empty() {
            parts.push(std::mem::take(&mut cur));
        }
        kind = k;
        if k != 0 {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    parts
}

/// ps5, v8, xr6, i30: letters read out (or as a word), digits as a number.
fn alphanumeric_phones(w: &str) -> Option<Phones> {
    let parts = alnum_parts(w);
    let is_digits = |p: &String| p.chars().all(|c| c.is_ascii_digit());
    if parts.is_empty() || !parts.iter().any(is_digits) || parts.iter().all(is_digits) {
        return None;
    }
    let mut out = Vec::new();
    for part in &parts {
        let got = if is_digits(part) {
            number_to_phones(part, false)
        } else {
            dict(part).or_else(|| letters_to_phones(part))
        }?;
        out.extend(got);
    }
    Some(out)
}

fn americanised(w: &str) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(w.to_string());
    let mut out = Vec::new();
    for (british, american) in BRITISH_ANYWHERE {
        if w.contains(british) {
            let cand = w.replace(british, american);
            if seen.insert(cand.clone()) {
                out.push(cand);
            }
        }
    }
    let cand = re_ending().replace(w, "${1}er${2}").into_owned();
    if seen.insert(cand.clone()) {
        out.push(cand);
    }
    let mut both = w.to_string();
    for (british, american) in BRITISH_ANYWHERE {
        both = both.replace(british, american);
    }
    let both = re_ending().replace(&both, "${1}er${2}").into_owned();
    if seen.insert(both.clone()) {
        out.push(both);
    }
    out
}

fn spelling_variants(w: &str) -> Vec<String> {
    let mut out = Vec::new();
    let n = clen(w);
    if n > 3 && w.ends_with("nt") {
        out.push(format!("{}'t", cslice(w, 0, n - 1)));
    }
    for suffix in CONTRACTION_ENDINGS {
        if n > suffix.len() && w.ends_with(suffix) {
            out.push(format!("{}'{suffix}", cslice(w, 0, n - suffix.len())));
        }
    }
    for (british, american) in BRITISH_ENDINGS {
        if n > british.len() + 1 && w.ends_with(british) {
            out.push(format!("{}{american}", cslice(w, 0, n - british.len())));
        }
    }
    if n > 4 && w.ends_with("re") {
        let third = w.chars().nth(n - 3).unwrap_or('a');
        if !"aeiou".contains(third) {
            out.push(format!("{}er", cslice(w, 0, n - 2)));
        }
    }
    out
}

/// `re.sub(r"(.)\1{2,}", repl, w)` with `repl` one or two copies of the char.
fn squash_runs(w: &str, keep: usize) -> String {
    let chars: Vec<char> = w.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let mut j = i;
        while j < chars.len() && chars[j] == chars[i] {
            j += 1;
        }
        let run = j - i;
        if run >= 3 {
            for _ in 0..keep {
                out.push(chars[i]);
            }
        } else {
            for _ in 0..run {
                out.push(chars[i]);
            }
        }
        i = j;
    }
    out
}

/// `re.fullmatch(r"(.{2,3}?)\1{1,}", w)`: the repeated syllable.
fn repeated_syllable(w: &str) -> Option<String> {
    let chars: Vec<char> = w.chars().collect();
    for unit in 2..=3 {
        if chars.len() >= unit * 2 && chars.len().is_multiple_of(unit) {
            let head = &chars[..unit];
            if chars.chunks(unit).all(|c| c == head) {
                return Some(head.iter().collect());
            }
        }
    }
    None
}

fn ordinal_digits(w: &str) -> Option<&str> {
    for suf in ["st", "nd", "rd", "th"] {
        if let Some(d) = w.strip_suffix(suf) {
            if !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()) {
                return Some(d);
            }
        }
    }
    None
}

/// Phonemes from the override table or CMU, with spelling variants and the
/// special tables as fallbacks.
fn direct(w: &str) -> Option<Phones> {
    let t = tables();
    if let Some(p) = t.overrides.get(w) {
        return Some(p.clone());
    }
    if let Some(p) = dict(w) {
        return Some(p);
    }
    for variant in spelling_variants(w) {
        if let Some(p) = dict(&variant) {
            return Some(p);
        }
    }
    if let Some(p) = t.said_as_word.get(w) {
        return Some(p.clone());
    }
    if !w.is_empty() && w.chars().all(|c| c.is_ascii_digit()) {
        return number_to_phones(w, false);
    }
    if let Some(d) = ordinal_digits(w) {
        return number_to_phones(d, true);
    }
    if t.spelled_out.contains(w) {
        return letters_to_phones(w);
    }
    for keep in [1, 2] {
        let squashed = squash_runs(w, keep);
        if squashed == w {
            continue;
        }
        if let Some(p) = t.overrides.get(&squashed).or_else(|| t.said_as_word.get(&squashed)) {
            return Some(p.clone());
        }
        if let Some(p) = dict(&squashed) {
            return Some(p);
        }
    }
    if let Some(unit) = repeated_syllable(w) {
        if let Some(p) = dict(&unit) {
            return Some(p);
        }
        if let Some(p) = t.said_as_word.get(&unit) {
            return Some(p.clone());
        }
    }
    alphanumeric_phones(w)
}

const SUFFIX_PHONES: [(&str, &[&str]); 11] = [
    ("iest", &["IY", "AH", "S", "T"]),
    ("est", &["AH", "S", "T"]),
    ("ier", &["IY", "ER"]),
    ("iness", &["IY", "N", "AH", "S"]),
    ("ness", &["N", "AH", "S"]),
    ("ily", &["AH", "L", "IY"]),
    ("ly", &["L", "IY"]),
    ("able", &["AH", "B", "AH", "L"]),
    ("ible", &["AH", "B", "AH", "L"]),
    ("ish", &["IH", "SH"]),
    ("y", &["IY"]),
];

const DOUBLE: &str = "bdfglmnprstz";

const PREFIX_PHONES: [(&str, &[&str]); 7] = [
    ("un", &["AH", "N"]),
    ("re", &["R", "IY"]),
    ("over", &["OW", "V", "ER"]),
    ("under", &["AH", "N", "D", "ER"]),
    ("non", &["N", "AA", "N"]),
    ("mis", &["M", "IH", "S"]),
    ("pre", &["P", "R", "IY"]),
];

fn s(v: &[&str]) -> Phones {
    v.iter().map(|x| x.to_string()).collect()
}

fn bases_for(stem: &str) -> Vec<String> {
    let mut out = vec![stem.to_string(), format!("{stem}e")];
    let ch: Vec<char> = stem.chars().collect();
    let n = ch.len();
    if n > 2 && ch[n - 1] == ch[n - 2] && DOUBLE.contains(ch[n - 1]) {
        out.push(cslice(stem, 0, n - 1).to_string());
    }
    if let Some(base) = stem.strip_suffix('i') {
        out.push(format!("{base}y"));
    }
    out
}

fn voiced_ending(phones: &[String]) -> Phones {
    let last = phones.last().map(String::as_str).unwrap_or("");
    if last == "T" || last == "D" {
        return s(&["AH", "D"]);
    }
    if ["P", "K", "F", "S", "SH", "CH", "TH"].contains(&last) {
        s(&["T"])
    } else {
        s(&["D"])
    }
}

fn suffixed(w: &str) -> Option<Phones> {
    let n = clen(w);
    for suffix in ["ed", "ing"] {
        if n > suffix.len() + 2 && w.ends_with(suffix) {
            let stem = cslice(w, 0, n - suffix.len());
            let sc: Vec<char> = stem.chars().collect();
            let m = sc.len();
            if m > 2 && sc[m - 1] == sc[m - 2] && DOUBLE.contains(sc[m - 1]) {
                if let Some(mut got) = direct(cslice(stem, 0, m - 1)) {
                    if suffix == "ing" {
                        got.extend(s(&["IH", "NG"]));
                    } else {
                        let e = voiced_ending(&got);
                        got.extend(e);
                    }
                    return Some(got);
                }
            }
        }
    }
    for (suffix, phones) in SUFFIX_PHONES {
        if n <= suffix.len() + 2 || !w.ends_with(suffix) {
            continue;
        }
        let stem = cslice(w, 0, n - suffix.len());
        for base in bases_for(stem) {
            if let Some(mut got) = direct(&base) {
                if phones[0] == "IY" && got.last().map(String::as_str) == Some("IY") {
                    got.extend(s(&phones[1..]));
                } else {
                    got.extend(s(phones));
                }
                return Some(got);
            }
        }
    }
    None
}

fn resolve_simple(w: &str) -> Option<Phones> {
    if let Some(r) = direct(w) {
        return Some(r);
    }
    let n = clen(w);
    let s_after = |bp: &Phones| -> &'static str {
        let last = bp.last().map(String::as_str).unwrap_or("");
        if ["P", "T", "K", "F", "TH"].contains(&last) {
            "S"
        } else {
            "Z"
        }
    };
    if n > 3 && w.ends_with('s') {
        if w.ends_with("es") {
            if let Some(mut bp) = base_phones(cslice(w, 0, n - 2)) {
                let last = bp.last().cloned().unwrap_or_default();
                if ["S", "Z", "SH", "ZH", "CH", "JH"].contains(&last.as_str()) {
                    bp.extend(s(&["IH", "Z"]));
                } else {
                    bp.push(s_after(&bp).to_string());
                }
                return Some(bp);
            }
        }
        if let Some(mut bp) = base_phones(cslice(w, 0, n - 1)) {
            bp.push(s_after(&bp).to_string());
            return Some(bp);
        }
    }
    if n > 4 && w.ends_with("ing") {
        let stem = cslice(w, 0, n - 3);
        if let Some(mut bp) = base_phones(stem).or_else(|| base_phones(&format!("{stem}e"))) {
            bp.extend(s(&["IH", "NG"]));
            return Some(bp);
        }
    }
    if n > 3 && w.ends_with("ed") {
        let stem = cslice(w, 0, n - 2);
        let bp = base_phones(stem)
            .or_else(|| base_phones(&format!("{stem}e")))
            .or_else(|| base_phones(cslice(w, 0, n - 1)));
        if let Some(mut bp) = bp {
            let last = bp.last().cloned().unwrap_or_default();
            if last == "T" || last == "D" {
                bp.extend(s(&["IH", "D"]));
            } else if ["P", "K", "F", "S", "SH", "CH", "TH"].contains(&last.as_str()) {
                bp.push("T".into());
            } else {
                bp.push("D".into());
            }
            return Some(bp);
        }
    }
    None
}

fn compute(word: &str) -> Option<Phones> {
    let w = word.to_lowercase();
    if let Some(r) = resolve_simple(&w) {
        return Some(r);
    }
    for cand in americanised(&w) {
        if let Some(r) = resolve_simple(&cand) {
            return Some(r);
        }
    }
    if let Some(r) = suffixed(&w) {
        return Some(r);
    }
    let n = clen(&w);
    for (prefix, phones) in PREFIX_PHONES {
        if n > prefix.len() + 2 && w.starts_with(prefix) {
            let rest = cslice(&w, prefix.len(), n);
            if let Some(got) = resolve_simple(rest).or_else(|| suffixed(rest)) {
                let mut out = s(phones);
                out.extend(got);
                return Some(out);
            }
        }
    }
    // Compound: the longest first half that is a word with a word after it.
    let mut i = n as i64 - 2;
    while i > 1 {
        let (a, b) = (direct(cslice(&w, 0, i as usize)), direct(cslice(&w, i as usize, n)));
        if let (Some(mut a), Some(b)) = (a, b) {
            a.extend(b);
            return Some(a);
        }
        i -= 1;
    }
    None
}

/// ARPAbet phonemes (no stress digits) for `word`, or `None`.
pub fn word_to_phonemes(word: &str) -> Option<Phones> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Phones>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().unwrap().get(word) {
        return hit.clone();
    }
    let got = compute(word);
    let mut c = cache.lock().unwrap();
    if c.len() > 100_000 {
        c.clear();
    }
    c.insert(word.to_string(), got.clone());
    got
}

const SPELL: &[(&str, &[&str])] = &[
    ("sch", &["SH"]),
    ("tch", &["CH"]),
    ("igh", &["AY"]),
    ("ough", &["AH", "F"]),
    ("ch", &["CH"]),
    ("sh", &["SH"]),
    ("th", &["TH"]),
    ("ph", &["F"]),
    ("ck", &["K"]),
    ("ng", &["NG"]),
    ("qu", &["K", "W"]),
    ("wh", &["W"]),
    ("gh", &["G"]),
    ("kn", &["N"]),
    ("wr", &["R"]),
    ("ps", &["S"]),
    ("ee", &["IY"]),
    ("ea", &["IY"]),
    ("oo", &["UW"]),
    ("ou", &["AW"]),
    ("ow", &["AW"]),
    ("ai", &["EY"]),
    ("ay", &["EY"]),
    ("oi", &["OY"]),
    ("oy", &["OY"]),
    ("au", &["AO"]),
    ("aw", &["AO"]),
    ("ei", &["EY"]),
    ("ie", &["IY"]),
    ("ue", &["UW"]),
    ("ar", &["AA", "R"]),
    ("er", &["ER"]),
    ("ir", &["ER"]),
    ("ur", &["ER"]),
    ("or", &["AO", "R"]),
    ("a", &["AE"]),
    ("b", &["B"]),
    ("c", &["K"]),
    ("d", &["D"]),
    ("e", &["EH"]),
    ("f", &["F"]),
    ("g", &["G"]),
    ("h", &["HH"]),
    ("i", &["IH"]),
    ("j", &["JH"]),
    ("k", &["K"]),
    ("l", &["L"]),
    ("m", &["M"]),
    ("n", &["N"]),
    ("o", &["AA"]),
    ("p", &["P"]),
    ("q", &["K"]),
    ("r", &["R"]),
    ("s", &["S"]),
    ("t", &["T"]),
    ("u", &["AH"]),
    ("v", &["V"]),
    ("w", &["W"]),
    ("x", &["K", "S"]),
    ("y", &["IY"]),
    ("z", &["Z"]),
];

/// A pronunciation for a word nothing in the dictionary covers: real chunks
/// first, then letter clusters.
pub fn guess_phonemes(word: &str) -> Option<Phones> {
    let w: String = word.to_lowercase().chars().filter(|c| c.is_ascii_lowercase()).collect();
    if w.is_empty() {
        return None;
    }
    let n = w.len();
    let mut out: Phones = Vec::new();
    let mut i = 0;
    while i < n {
        let mut chunk: Option<(Phones, usize)> = None;
        let mut j = n;
        while j > i + 2 {
            if let Some(got) = direct(&w[i..j]) {
                chunk = Some((got, j));
                break;
            }
            j -= 1;
        }
        if let Some((got, j)) = chunk {
            out.extend(got);
            i = j;
            continue;
        }
        match SPELL.iter().find(|(cluster, _)| w[i..].starts_with(cluster)) {
            Some((cluster, phones)) => {
                out.extend(s(phones));
                i += cluster.len();
            }
            None => i += 1,
        }
    }
    (!out.is_empty()).then_some(out)
}

/// ZH has no corpus coverage: SH stands in for it.
pub fn equiv(p: &str) -> &str {
    if p == "ZH" {
        "SH"
    } else {
        p
    }
}

/// What the splicer believes a source word contains.
pub fn phones_of(word: &str) -> Option<Phones> {
    word_to_phonemes(&word.to_lowercase())
        .map(|raw| raw.iter().map(|p| equiv(p).to_string()).collect())
}

/// The phonemes a splice aims at after ZH->SH; `loose` guesses for unknown words.
pub fn canonical_phones(word: &str, strict: bool) -> Option<Phones> {
    let mut raw = word_to_phonemes(word);
    if raw.is_none() && !strict {
        raw = guess_phonemes(word);
    }
    raw.map(|r| r.iter().map(|p| equiv(p).to_string()).collect())
}
