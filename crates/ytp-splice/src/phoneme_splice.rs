//! Building a word the corpus never said out of pieces of words it did:
//! the fewest source units covering the target's phonemes (a DP), then each
//! unit cut at an aligned phoneme boundary (`find_phoneme_splice` and
//! `_realise` in `app/phonemes.py`).
//!
//! Cuts use the alignments stored in the corpus; a clip with none cannot be
//! cut and is used whole (the Python app aligns it with a neural model).

use std::collections::HashMap;
use std::sync::Arc;

use ytp_text::phonemes::{equiv, tables};
use ytp_text::{guess_phonemes, is_vowel, word_to_phonemes};

use crate::align::{cut_end_after_phonemes, cut_start_before_phonemes};
use crate::audio::{rms_windows, snap_to_zero_crossing, AudioCache, SR};
use crate::model::{Segment, Unit};
use crate::pool::Pool;
use crate::prosody::ToneCtx;
use crate::recipe::{self, Group};
use crate::rng::PyRandom;
use crate::select::pick_clip;

const FUNCTION_WORDS: &[&str] = &[
    "a", "an", "the", "to", "of", "and", "is", "it", "in", "on", "or", "as", "at", "but", "for",
    "so", "if", "by", "be", "he", "we", "do", "my", "me", "you", "your", "his", "her", "i", "am",
    "are", "was", "that", "this", "with", "they", "them", "then", "than",
];
const SIMILAR_GROUPS: &[&[&str]] = &[
    &["IY", "IH"], &["IH", "EH"], &["EY", "EH"], &["EH", "AE"], &["AE", "AA"],
    &["AA", "AO"], &["AO", "OW"], &["OW", "UH"], &["UH", "UW"], &["AH", "ER"],
    &["AH", "AA"], &["AY", "AA"], &["AW", "AA"], &["OY", "AO"],
    &["P", "B"], &["T", "D"], &["K", "G"], &["F", "V"], &["S", "Z"],
    &["SH", "ZH"], &["CH", "JH"], &["TH", "DH"],
    &["M", "N"], &["N", "NG"], &["L", "R"], &["S", "SH"], &["Z", "ZH"],
    &["CH", "SH"], &["JH", "ZH"], &["TH", "F"], &["DH", "V"], &["W", "V"],
    &["HH", "F"], &["Y", "IY"], &["R", "ER"],
];
const SUB_COST: f64 = 10.0;
const SKIP_COST: f64 = 25.0;
const HALF_COST: f64 = 2.0;
const AFFRICATE_SPLIT: f64 = 0.45;
const MIN_PER_PHONE: f64 = 0.05;
const VOWEL_EXTRA: f64 = 0.05;
const TRIM_FLOOR: f64 = 0.18;
const TRIM_KEEP: f64 = 0.03;
const TRIM_MIN: f64 = 0.045;
const MIN_UNIT: f64 = 0.06;

/// Phonemes that may stand in for `phone` (each one feature away).
fn near(phone: &str) -> &'static [&'static str] {
    static T: std::sync::OnceLock<HashMap<&'static str, Vec<&'static str>>> =
        std::sync::OnceLock::new();
    let t = T.get_or_init(|| {
        let mut m: HashMap<&'static str, Vec<&'static str>> = HashMap::new();
        for g in SIMILAR_GROUPS {
            for p in g.iter() {
                let e = m.entry(*p).or_default();
                e.extend(g.iter().copied());
            }
        }
        for (p, v) in m.iter_mut() {
            v.sort_unstable();
            v.dedup();
            v.retain(|x| x != p);
        }
        m
    });
    t.get(phone).map_or(&[], |v| v.as_slice())
}

fn affricate_half(p: &str) -> Option<&'static str> {
    match p {
        "CH" | "JH" => Some("SH"),
        _ => None,
    }
}

type Penalty = HashMap<i64, i64>;

/// A source word and where a phoneme sits in it.
#[derive(Clone)]
struct Entry<'a> {
    word: &'a str,
    phones: Arc<Vec<String>>,
    clips: &'a [usize],
    pos: usize,
}

type Index<'a> = HashMap<String, Vec<Entry<'a>>>;

#[derive(Clone, Debug)]
enum Flag {
    Half,
    Verbatim,
    Slice(f64, f64),
    /// Cut by hand: exactly these times of the clip.
    Span(f64, f64),
}

#[derive(Clone, Debug)]
struct Chosen {
    tstart: usize,
    matched: usize,
    word: String,
    clip: usize,
    pos: usize,
    total: usize,
    flag: Option<Flag>,
}

pub struct SpliceCtx<'a> {
    pub pool: &'a Pool,
    pub tone: Option<&'a ToneCtx>,
    pub audio: &'a AudioCache<'a>,
}

impl SpliceCtx<'_> {
    fn pick(&self, rng: &mut PyRandom, rows: &[usize], word: &str) -> Option<usize> {
        pick_clip(self.pool, self.tone, rng, rows, word, true)
    }

    /// `_best_clip`: not one the user rated down for this target if possible.
    fn best_clip(&self, rng: &mut PyRandom, clips: &[usize], word: &str, penalty: &Penalty) -> usize {
        if !penalty.is_empty() {
            let clean: Vec<usize> = clips
                .iter()
                .copied()
                .filter(|&i| {
                    self.pool.clips[i]
                        .id
                        .map_or(0, |id| penalty.get(&id).copied().unwrap_or(0))
                        <= 0
                })
                .collect();
            if !clean.is_empty() {
                if let Some(c) = self.pick(rng, &clean, word) {
                    return c;
                }
            }
        }
        match self.pick(rng, clips, word) {
            Some(c) => c,
            None => clips[rng.choice_index(clips.len())],
        }
    }
}

fn clip_floor(phones: &[String]) -> f64 {
    let mut need = MIN_PER_PHONE * phones.len() as f64;
    if phones.iter().any(|p| is_vowel(p)) {
        need += VOWEL_EXTRA;
    }
    need
}

/// Clips long enough to hold `phones` first, shortest of those first.
fn by_audibility(pool: &Pool, clips: &[usize], phones: &[String]) -> Vec<usize> {
    let floor = clip_floor(phones);
    let mut v = clips.to_vec();
    v.sort_by(|&a, &b| {
        let key = |i: usize| {
            let d = pool.clips[i].duration();
            if d >= floor {
                (0, d)
            } else {
                (1, -d)
            }
        };
        let (ka, kb) = (key(a), key(b));
        ka.0.cmp(&kb.0)
            .then(ka.1.partial_cmp(&kb.1).unwrap_or(std::cmp::Ordering::Equal))
    });
    v
}

/// Where the sound in a span actually stops.
fn audible_end(audio: &AudioCache, source: &str, start: f64, end: f64) -> f64 {
    let dur = end - start;
    if dur <= TRIM_MIN {
        return end;
    }
    audio.memo_ends(1, source, start, end, || {
        let Some(x) = audio.probe.decode(source, start, dur) else {
            return end;
        };
        if x.len() < 160 {
            return end;
        }
        let env = rms_windows(&x, 160);
        if env.is_empty() {
            return end;
        }
        let peak = env.iter().cloned().fold(f64::MIN, f64::max);
        if peak <= 0.0 {
            return end;
        }
        let floor = peak * TRIM_FLOOR;
        let mut last = 0usize;
        for (i, l) in env.iter().enumerate() {
            if *l >= floor {
                last = i;
            }
        }
        let trimmed = start + (last + 1) as f64 * (160.0 / SR) + TRIM_KEEP;
        (start + TRIM_MIN).max(end.min(trimmed))
    })
}

/// `t` moved to the nearest zero crossing of matching slope.
fn snap_time(audio: &AudioCache, source: &str, t: f64) -> f64 {
    let idx = (t * SR).round_ties_even() as i64;
    let s0 = (idx - 128).max(0);
    let Some(x) = audio.probe.decode(source, s0 as f64 / SR, 384.0 / SR) else {
        return t;
    };
    if x.len() < 2 {
        return t;
    }
    (s0 + snap_to_zero_crossing(&x, idx - s0, 64)) as f64 / SR
}

fn phones_for(w: &str) -> Option<Vec<String>> {
    let t = tables();
    if let Some(p) = t.overrides.get(w) {
        return Some(p.iter().map(|x| equiv(x).to_string()).collect());
    }
    ytp_text::cmudict::get(w).map(|p| p.iter().map(|x| equiv(x).to_string()).collect())
}

fn find_sub(cph: &[String], group: &[String]) -> Option<usize> {
    if group.len() > cph.len() {
        return None;
    }
    (0..=cph.len() - group.len()).find(|&pos| cph[pos..pos + group.len()] == *group)
}

/// Clips of the last of `words` spoken right after the others in one source.
fn phrase_clips(pool: &Pool, words: &[&str]) -> Vec<usize> {
    let mut res = Vec::new();
    for (src, idx) in pool.word_positions.get(words[0]).into_iter().flatten() {
        let Some(seq) = pool.ordered_by_source.get(src) else { continue };
        if (0..words.len()).all(|k| seq.get(idx + k).is_some_and(|&c| pool.clips[c].word == words[k])) {
            let c = seq[idx + words.len() - 1];
            if pool.clips[c].duration() > 0.04 {
                res.push(c);
            }
        }
    }
    res
}

#[allow(clippy::too_many_arguments)]
fn chosen_from_recipe(
    cx: &SpliceCtx,
    rng: &mut PyRandom,
    recipe: &[Group],
    index: &Index,
    penalty: &Penalty,
) -> Option<Vec<Chosen>> {
    let mut chosen = Vec::new();
    let mut t = 0;
    for Group { phones: group, from: preferred, cut } in recipe {
        let mut unit: Option<Chosen> = None;
        if let (Some(cut), Some(first)) = (cut, preferred.first()) {
            // That clip, those times, but only while the clip is still what it
            // was: the right word, and the times inside it.
            for e in index.get(&group[0]).into_iter().flatten() {
                if e.word != first || e.phones.get(e.pos..e.pos + group.len()) != Some(group.as_slice()) {
                    continue;
                }
                let found = e.clips.iter().copied().find(|&c| cx.pool.clips[c].id == Some(cut.clip_id));
                if let Some(clip) = found {
                    let c = &cx.pool.clips[clip];
                    if c.start_time - 0.001 <= cut.start && cut.start < cut.end && cut.end <= c.end_time + 0.001 {
                        unit = Some(Chosen {
                            tstart: t,
                            matched: group.len(),
                            word: e.word.to_string(),
                            clip,
                            pos: e.pos,
                            total: e.phones.len(),
                            flag: Some(Flag::Span(cut.start, cut.end)),
                        });
                    }
                }
                break;
            }
        }
        for pref in if unit.is_some() { &[][..] } else { preferred.as_slice() } {
            if pref.contains(' ') {
                let words: Vec<&str> = pref.split(' ').collect();
                let w = words[words.len() - 1];
                let cph = phones_for(w);
                let pos = cph.as_ref().and_then(|c| find_sub(c, group));
                let clips = if pos.is_some() { phrase_clips(cx.pool, &words) } else { Vec::new() };
                if let (Some(pos), false) = (pos, clips.is_empty()) {
                    let clip = cx.best_clip(rng, &clips, w, penalty);
                    unit = Some(Chosen {
                        tstart: t,
                        matched: group.len(),
                        word: w.to_string(),
                        clip,
                        pos,
                        total: cph.map_or(0, |c| c.len()),
                        flag: Some(Flag::Verbatim),
                    });
                    break;
                }
            } else {
                let mut name = pref.as_str();
                let mut flag = None;
                if let Some(rest) = pref.strip_prefix('*') {
                    let (n, range) = rest.split_once(':').map_or((rest, None), |(n, r)| (n, Some(r)));
                    name = n;
                    flag = Some(match range.and_then(|r| r.split_once('-')) {
                        Some((lo, hi)) => match (lo.parse(), hi.parse()) {
                            (Ok(lo), Ok(hi)) => Flag::Slice(lo, hi),
                            _ => Flag::Verbatim,
                        },
                        None => Flag::Verbatim,
                    });
                }
                for e in index.get(&group[0]).into_iter().flatten() {
                    if e.word == name && e.phones[e.pos..].starts_with(group) {
                        let clip = cx.best_clip(rng, e.clips, e.word, penalty);
                        unit = Some(Chosen {
                            tstart: t,
                            matched: group.len(),
                            word: e.word.to_string(),
                            clip,
                            pos: e.pos,
                            total: e.phones.len(),
                            flag: flag.clone(),
                        });
                        break;
                    }
                }
                if unit.is_some() {
                    break;
                }
            }
        }
        if unit.is_none() {
            let mut cands: Vec<(usize, &Entry)> = Vec::new();
            for e in index.get(&group[0]).into_iter().flatten() {
                if e.phones[e.pos..].starts_with(group) {
                    let ncuts = usize::from(e.pos > 0)
                        + usize::from(e.pos + group.len() < e.phones.len());
                    cands.push((ncuts, e));
                }
            }
            if cands.is_empty() {
                return None;
            }
            cands.sort_by_key(|c| c.0);
            let e = cands[0].1;
            let clip = cx.best_clip(rng, e.clips, e.word, penalty);
            unit = Some(Chosen {
                tstart: t,
                matched: group.len(),
                word: e.word.to_string(),
                clip,
                pos: e.pos,
                total: e.phones.len(),
                flag: None,
            });
        }
        chosen.push(unit?);
        t += group.len();
    }
    Some(chosen)
}

/// Cover `target_word`'s phonemes with the fewest source units; the playable
/// segments, or `None` when the corpus cannot say it in this `mode`.
pub fn find_phoneme_splice(
    cx: &SpliceCtx,
    rng: &mut PyRandom,
    target_word: &str,
    penalty: &Penalty,
    mode: &str,
    max_units: usize,
) -> Option<Vec<Segment>> {
    let _pronunciations = cx.pool.scope();
    let mut raw = word_to_phonemes(target_word);
    if raw.is_none() && mode != "strict" {
        raw = guess_phonemes(target_word);
    }
    let raw = raw?;
    let mut approximate = mode != "strict" && word_to_phonemes(target_word).is_none();
    let target: Vec<String> = raw.iter().map(|p| equiv(p).to_string()).collect();
    let n = target.len();

    let mut index: Index = HashMap::new();
    let mut half_index: Index = HashMap::new();
    for (word, clips) in &cx.pool.by_word {
        let Some(raw) = word_to_phonemes(&word.to_lowercase()) else { continue };
        let cphones: Arc<Vec<String>> =
            Arc::new(raw.iter().map(|p| equiv(p).to_string()).collect());
        for pos in 0..cphones.len() {
            let e = Entry {
                word,
                phones: cphones.clone(),
                clips,
                pos,
            };
            if let Some(half) = affricate_half(&cphones[pos]) {
                half_index.entry(half.to_string()).or_default().push(e.clone());
            }
            index.entry(cphones[pos].clone()).or_default().push(e);
        }
    }

    // A recipe saved for this corpus wins over the built-in one.
    let key = target_word.to_lowercase();
    let saved = cx.pool.recipes.get(&key).filter(|r| !r.is_empty());
    let builtin = || tables().recipes.get(&key).map(|r| recipe::from_pairs(r));
    if let Some(recipe) = saved.cloned().or_else(builtin) {
        let flat: Vec<&String> = recipe.iter().flat_map(|g| g.phones.iter()).collect();
        if flat.len() == target.len() && flat.iter().zip(&target).all(|(a, b)| *a == b) {
            if let Some(chosen) = chosen_from_recipe(cx, rng, &recipe, &index, penalty) {
                return realise(cx, rng, &chosen, &target, &index, penalty, max_units);
            }
        }
    }

    enum Prev {
        Skip(usize),
        Unit(Chosen, usize),
    }
    let inf = f64::INFINITY;
    let mut dp = vec![inf; n + 1];
    let mut prev: Vec<Option<Prev>> = (0..=n).map(|_| None).collect();
    dp[0] = 0.0;

    for i in 0..n {
        if dp[i] == inf {
            continue;
        }
        let first = &target[i];
        let mut candidates: Vec<&Entry> = index.get(first).into_iter().flatten().collect();
        if mode != "strict" {
            for alt in near(first) {
                candidates.extend(index.get(*alt).into_iter().flatten());
            }
        }
        for e in candidates {
            let (mut matched, mut subs) = (0usize, 0usize);
            while e.pos + matched < e.phones.len() && i + matched < n {
                let (have, want) = (&e.phones[e.pos + matched], &target[i + matched]);
                if have == want {
                } else if mode != "strict" && near(want).contains(&have.as_str()) {
                    subs += 1;
                } else {
                    break;
                }
                matched += 1;
            }
            if matched == 0 {
                continue;
            }
            let front_cut = e.pos > 0;
            let end_cut = e.pos + matched < e.phones.len();
            let mut cost = dp[i] + 1.0 - 0.08 * matched as f64 + SUB_COST * subs as f64;
            if front_cut {
                cost += 0.12;
            }
            if end_cut {
                cost += 0.12;
            }
            cost -= 0.04 * e.clips.len().min(10) as f64 / 10.0;
            if FUNCTION_WORDS.iter().any(|f| f.eq_ignore_ascii_case(e.word))
                && e.phones[e.pos..e.pos + matched].iter().any(|p| is_vowel(p))
            {
                cost += 0.60;
            }
            if end_cut && is_vowel(&e.phones[e.pos + matched - 1]) {
                cost += 0.25;
            }
            if front_cut && is_vowel(&e.phones[e.pos - 1]) {
                cost += 0.25;
            }
            let clip = cx.best_clip(rng, e.clips, e.word, penalty);
            if !penalty.is_empty() {
                cost += 0.7 * cx.pool.clips[clip].id.map_or(0, |id| penalty.get(&id).copied().unwrap_or(0)) as f64;
            }
            let j = i + matched;
            if cost < dp[j] {
                dp[j] = cost;
                prev[j] = Some(Prev::Unit(
                    Chosen {
                        tstart: i,
                        matched,
                        word: e.word.to_string(),
                        clip,
                        pos: e.pos,
                        total: e.phones.len(),
                        flag: None,
                    },
                    subs,
                ));
            }
        }

        if mode != "strict" {
            for e in half_index.get(first).into_iter().flatten() {
                let clip = cx.best_clip(rng, e.clips, e.word, penalty);
                let mut cost = dp[i] + 1.0 + HALF_COST;
                if !penalty.is_empty() {
                    cost += 0.7 * cx.pool.clips[clip].id.map_or(0, |id| penalty.get(&id).copied().unwrap_or(0)) as f64;
                }
                if cost < dp[i + 1] {
                    dp[i + 1] = cost;
                    prev[i + 1] = Some(Prev::Unit(
                        Chosen {
                            tstart: i,
                            matched: 1,
                            word: e.word.to_string(),
                            clip,
                            pos: e.pos,
                            total: e.phones.len(),
                            flag: Some(Flag::Half),
                        },
                        0,
                    ));
                }
            }
        }

        if mode == "desperate" && dp[i] + SKIP_COST < dp[i + 1] {
            dp[i + 1] = dp[i] + SKIP_COST;
            prev[i + 1] = Some(Prev::Skip(i));
        }
    }

    if dp[n] == inf {
        return None;
    }
    let mut chosen: Vec<Chosen> = Vec::new();
    let mut p = n;
    while p > 0 {
        match prev[p].take()? {
            Prev::Skip(from) => {
                approximate = true;
                p = from;
            }
            Prev::Unit(unit, subs) => {
                if subs > 0 {
                    approximate = true;
                }
                p = unit.tstart;
                chosen.push(unit);
            }
        }
    }
    chosen.reverse();
    if chosen.is_empty() {
        return None;
    }
    let mut segs = realise(cx, rng, &chosen, &target, &index, penalty, max_units)?;
    if approximate {
        for s in &mut segs {
            s.approx = true;
        }
    }
    Some(segs)
}

/// Playable segments from a chosen unit list.
fn realise(
    cx: &SpliceCtx,
    rng: &mut PyRandom,
    chosen: &[Chosen],
    target: &[String],
    index: &Index,
    penalty: &Penalty,
    max_units: usize,
) -> Option<Vec<Segment>> {
    if chosen.len() > max_units {
        return None;
    }
    let pool = cx.pool;
    let audio = cx.audio;

    // One unit realised from a source word, trimmed to its aligned content.
    let try_unit = |w: &str, clip_list: &[usize], ps: usize, total: usize, sub: &[String], last_v: bool| -> Option<Segment> {
        let front_cut = ps > 0;
        let end_cut = ps + sub.len() < total;
        let cut = front_cut || end_cut;
        for &ci in clip_list.iter().take(6) {
            let cand = &pool.clips[ci];
            let dur = cand.duration();
            let cs = cut_start_before_phonemes(pool, cand, ps);
            let ce = cut_end_after_phonemes(pool, cand, ps + sub.len(), last_v);
            let (Some(mut cs), Some(mut ce)) = (cs, ce) else { continue };
            if ce - cs <= 0.04 {
                continue;
            }
            if !last_v && !end_cut {
                ce = (ce + 0.06).min(dur);
            }
            if !front_cut {
                cs = (cs - 0.02).max(0.0);
            }
            cs = cs.min((dur - 0.02).max(0.0)).max(0.0);
            ce = (cs + 0.02).max(ce.min(dur));
            let mut s = Segment::from_clip(cand);
            s.src_start = Some(cand.start_time);
            s.src_end = Some(cand.end_time);
            s.start_time = cand.start_time + cs;
            s.end_time = cand.start_time + ce;
            s.end_time = audible_end(audio, &cand.source_file, s.start_time, s.end_time);
            if cut {
                if front_cut {
                    s.start_time = snap_time(audio, &cand.source_file, s.start_time);
                }
                if end_cut {
                    let snapped = snap_time(audio, &cand.source_file, s.end_time);
                    if snapped > s.start_time + 0.02 {
                        s.end_time = snapped;
                    }
                }
            }
            s.next_start = Some(s.end_time);
            s.subword = cut;
            s.cut = cut;
            s.spliced_from = Some(w.to_string());
            s.matched = Some(sub.len());
            return Some(s);
        }
        None
    };

    let sources_for = |sub: &[String]| -> Vec<(usize, &str, &[usize], usize, usize)> {
        let mut res = Vec::new();
        for e in index.get(&sub[0]).into_iter().flatten() {
            if e.phones[e.pos..].starts_with(sub) {
                let ncuts = usize::from(e.pos > 0) + usize::from(e.pos + sub.len() < e.phones.len());
                res.push((ncuts, e.word, e.clips, e.pos, e.phones.len()));
            }
        }
        res.sort_by_key(|r| r.0);
        res
    };

    let mut segments: Vec<Segment> = Vec::new();
    for u in chosen {
        let sub = &target[u.tstart..u.tstart + u.matched];
        let last_v = is_vowel(&sub[sub.len() - 1]);
        let cclip = &pool.clips[u.clip];

        if matches!(u.flag, Some(Flag::Half)) {
            let mut seg = None;
            let list: Vec<usize> = pool.by_word.get(&u.word).cloned().unwrap_or_else(|| vec![u.clip]);
            for &ci in list.iter().take(6) {
                let cand = &pool.clips[ci];
                let a = cut_start_before_phonemes(pool, cand, u.pos);
                let b = cut_end_after_phonemes(pool, cand, u.pos + 1, false);
                let (Some(a), Some(b)) = (a, b) else { continue };
                if b - a <= 0.03 {
                    continue;
                }
                let st = a + (b - a) * AFFRICATE_SPLIT;
                if b - st <= 0.02 {
                    continue;
                }
                let mut s = Segment::from_clip(cand);
                s.start_time = cand.start_time + st;
                s.end_time = cand.start_time + b;
                s.start_time = snap_time(audio, &cand.source_file, s.start_time);
                s.subword = true;
                s.cut = true;
                s.spliced_from = Some(u.word.clone());
                s.matched = Some(1);
                seg = Some(s);
                break;
            }
            segments.push(seg?);
            continue;
        }

        if let Some(Flag::Span(start, end)) = u.flag {
            // Times set by hand against the waveform: taken exactly, and
            // marked edited so the encoder pads nothing.
            let mut s = Segment::from_clip(cclip);
            s.start_time = start;
            s.end_time = end;
            s.src_start = Some(cclip.start_time);
            s.src_end = Some(cclip.end_time);
            s.subword = true;
            s.cut = true;
            s.edited = true;
            s.spliced_from = Some(u.word.clone());
            s.matched = Some(u.matched);
            segments.push(s);
            continue;
        }

        if let Some(flag) = &u.flag {
            let mut s = Segment::from_clip(cclip);
            let sliced = matches!(flag, Flag::Slice(..));
            if let Flag::Slice(lo, hi) = flag {
                let cdur = cclip.duration();
                let dur = rng.uniform(*lo, *hi).min(cdur);
                let span = cdur - dur;
                let st = cclip.start_time + if span > 0.0 { rng.uniform(0.0, span) } else { 0.0 };
                s.start_time = st;
                s.end_time = st + dur;
                s.start_time = snap_time(audio, &cclip.source_file, s.start_time);
                let snap1 = snap_time(audio, &cclip.source_file, s.end_time);
                if snap1 > s.start_time + 0.02 {
                    s.end_time = snap1;
                }
            }
            s.subword = sliced;
            s.cut = sliced;
            s.spliced_from = Some(u.word.clone());
            s.matched = Some(u.matched);
            segments.push(s);
            continue;
        }

        let pool_list: Vec<usize> = pool.by_word.get(&u.word).cloned().unwrap_or_else(|| vec![u.clip]);
        let mut ordered = by_audibility(pool, &pool_list, sub);
        if !penalty.is_empty() {
            ordered.sort_by_key(|&i| pool.clips[i].id.map_or(0, |id| penalty.get(&id).copied().unwrap_or(0)));
        }
        let mut seg = try_unit(&u.word, &ordered, u.pos, u.total, sub, last_v);
        if seg.is_none() {
            for (_nc, w, cl, ps, total) in sources_for(sub) {
                if w == u.word {
                    continue;
                }
                seg = try_unit(w, &by_audibility(pool, cl, sub), ps, total, sub, last_v);
                if seg.is_some() {
                    break;
                }
            }
        }
        let seg = match seg {
            Some(s) => s,
            None => {
                // Last resort: the longest whole-word clip.
                let best = pool_list
                    .iter()
                    .copied()
                    .fold(None::<usize>, |b, i| match b {
                        Some(b) if pool.clips[b].duration() >= pool.clips[i].duration() => Some(b),
                        _ => Some(i),
                    })
                    .unwrap_or(u.clip);
                let bc = &pool.clips[best];
                let mut s = Segment::from_clip(bc);
                s.cut = false;
                s.src_start = Some(bc.start_time);
                s.src_end = Some(bc.end_time);
                s.end_time = audible_end(audio, &s.source_file, s.start_time, s.end_time);
                s.next_start = Some(s.end_time);
                s.spliced_from = Some(u.word.clone());
                s.matched = Some(u.matched);
                s
            }
        };
        segments.push(seg);
    }

    for (seg, u) in segments.iter_mut().zip(chosen) {
        seg.unit = Some(Unit {
            at: u.tstart,
            phones: target[u.tstart..u.tstart + u.matched].to_vec(),
            from: seg.spliced_from.clone().unwrap_or_else(|| u.word.clone()),
            clip_id: seg.id,
        });
    }

    for seg in &mut segments {
        if seg.end_time - seg.start_time >= 0.04 {
            continue;
        }
        let lo = seg.src_start.unwrap_or(seg.start_time);
        let hi = seg.src_end.unwrap_or(seg.end_time + MIN_UNIT);
        let need = MIN_UNIT - (seg.end_time - seg.start_time);
        let back = need.min(seg.start_time - lo);
        seg.start_time -= back.max(0.0);
        let still = MIN_UNIT - (seg.end_time - seg.start_time);
        if still > 0.0 {
            seg.end_time = hi.min(seg.end_time + still);
        }
        if seg.end_time - seg.start_time < 0.02 {
            seg.end_time = seg.start_time + 0.02;
        }
    }

    let last = segments.len().saturating_sub(1);
    for (idx, seg) in segments.iter_mut().enumerate() {
        seg.prev_end = Some(seg.start_time);
        if !(idx == last && !seg.cut) {
            seg.next_start = Some(seg.end_time);
        }
        seg.fade_in = Some(0.0);
        seg.fade_out = Some(0.0);
    }
    if let Some(first) = segments.first_mut() {
        first.fade_in = None;
    }
    if let Some(l) = segments.last_mut() {
        l.fade_out = None;
    }
    Some(segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn near_is_symmetric_enough() {
        assert!(near("P").contains(&"B"));
        assert!(!near("P").contains(&"P"));
        assert!(near("ZZ").is_empty());
    }
}
