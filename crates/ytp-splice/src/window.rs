//! Which span of source audio a segment pulls, how long each segment plays,
//! and when each word is on screen (`extract_window`, `segment_durations`
//! and `timeline` in `app/generate.py`).

use serde_json::{json, Value};

use crate::audio::AudioCache;
use crate::model::Segment;
use crate::options::GenerationOptions;

pub const PAD_START: f64 = 0.05;
pub const PAD_END: f64 = 0.18;
pub const PAD_END_LAST: f64 = 0.45;
pub const SAFETY: f64 = 0.05;
pub const TAIL_GAP: f64 = 0.05;
pub const BLEED_TAIL: f64 = 0.16;
pub const SONORANT_TAIL: f64 = 0.04;
pub const SONORANT_MAX: f64 = 0.16;
pub const SONORANT_QUIET: f64 = 0.22;
pub const TAIL_AIR: f64 = 0.03;
pub const LEAD_AIR: f64 = 0.015;
pub const AIR_QUIET: f64 = 0.1;
pub const AIR_FLOOR: f64 = 2.0;
pub const AIR_ROOM: f64 = 0.5;
pub const RING_MAX: f64 = 0.40;
pub const FPS: f64 = 25.0;
const FINAL_SONORANTS: [&str; 6] = ["M", "N", "NG", "L", "R", "ER"];
const HOP: f64 = 160.0 / 16000.0;

/// Python's `round(x)` (half to even) as an integer-valued float.
pub fn round_even(x: f64) -> f64 {
    x.round_ties_even()
}

/// Python's `round(x, 3)`.
pub fn round3(x: f64) -> f64 {
    round_even(x * 1000.0) / 1000.0
}

fn sound_ends(audio: &AudioCache, source: &str, start: f64, stored_end: f64) -> f64 {
    audio.memo_ends(0, source, start, stored_end, || {
        let limit = stored_end + SONORANT_MAX;
        let fallback = stored_end + SONORANT_TAIL;
        let Some(x) = audio.probe.decode(source, start, limit - start) else {
            return fallback;
        };
        let env = crate::audio::rms_windows(&x, 160);
        if env.is_empty() {
            return fallback;
        }
        let peak = env.iter().cloned().fold(f64::MIN, f64::max);
        if peak <= 0.0 {
            return fallback;
        }
        let floor = peak * SONORANT_QUIET;
        let i = ((stored_end - start) / HOP) as usize;
        let mut j = i;
        while j < env.len() && env[j] >= floor {
            j += 1;
        }
        let extra = (j - i) as f64 * HOP;
        limit.min(stored_end + SONORANT_TAIL.max(extra))
    })
}

/// Where the sound of a word stored at `start..end` begins and stops,
/// looking no further than `a` and `b`; `None` when the audio is unreadable.
fn heard(
    audio: &AudioCache,
    source: &str,
    a: f64,
    start: f64,
    end: f64,
    b: f64,
) -> Option<(f64, f64)> {
    let (a, b) = (round3(a), round3(b));
    let lo = round3((a - AIR_ROOM).max(0.0)).max(0.0);
    let env = audio.envelope(source, lo, round3(b + AIR_ROOM));
    let off = round_even((a - lo) / HOP) as usize;
    let first = off;
    let last = env.len().min(off + ((b - a) / HOP).ceil() as usize);
    let mut i = first.max((off as i64 + ((start - a) / HOP) as i64).max(0) as usize);
    let mut j = last.min((off as i64 + ((end - a) / HOP).ceil() as i64).max(0) as usize);
    if j <= i || j > env.len() {
        return None;
    }
    let peak = env[i..j].iter().cloned().fold(f64::MIN, f64::max);
    if peak <= 0.0 {
        return None;
    }
    let mut sorted: Vec<f64> = env.to_vec();
    sorted.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    let room = sorted[env.len() / 10];
    let quiet = (peak * AIR_QUIET).min((room * AIR_FLOOR).max(peak * 0.001));
    let still = |k: usize| k < env.len() && env[k] >= quiet;
    while i > first && (env[i - 1] >= quiet || (i > first + 1 && env[i - 2] >= quiet)) {
        i -= 1;
    }
    while j < last && (still(j) || still(j + 1)) {
        j += 1;
    }
    Some((lo + i as f64 * HOP, lo + j as f64 * HOP))
}

/// Does this clip end on a sound that fades rather than stops?
fn ends_in_sonorant(seg: &Segment) -> bool {
    let Some(last) = seg.word.split_whitespace().last() else {
        return false;
    };
    ytp_text::word_to_phonemes(&last.to_lowercase())
        .and_then(|p| p.last().cloned())
        .is_some_and(|p| FINAL_SONORANTS.contains(&p.as_str()))
}

/// The span of source audio to pull for `seg`: `(start, end)`.
pub fn extract_window(seg: &Segment, pad_end: f64, audio: &AudioCache) -> (f64, f64) {
    let edited = seg.edited;
    let mut start = seg.start_time - if edited { 0.0 } else { PAD_START };
    if let Some(p) = seg.prev_end {
        start = start.max(p + SAFETY);
    }
    start = start.min(seg.start_time).max(0.0);

    let stored_end = seg.end_time;
    let next_start = seg.next_start;
    let butt = next_start.is_some_and(|n| n <= stored_end + 1e-3);

    let mut want = if edited { stored_end } else { stored_end + pad_end };
    if !edited && !seg.subword && ends_in_sonorant(seg) {
        want = want.max(sound_ends(audio, &seg.source_file, seg.start_time, stored_end));
    }
    if !edited && !seg.subword {
        let reach = want.max(stored_end + RING_MAX);
        if let Some((onset, stop)) =
            heard(audio, &seg.source_file, start, seg.start_time, stored_end, reach)
        {
            start = start.max(seg.start_time.min(onset - LEAD_AIR));
            want = reach.min(stored_end.max(stop + TAIL_AIR));
        }
    }
    if let Some(n) = next_start {
        let gap = if seg.bleed_risk { BLEED_TAIL } else { TAIL_GAP };
        want = want.min(stored_end.max(n - gap));
    }
    let floor = if butt || edited {
        stored_end
    } else {
        stored_end.max(start + 0.05)
    };
    (start, want.max(floor))
}

/// `(source span, sounding length, total output length)` of one segment.
pub fn segment_durations(
    seg: &Segment,
    window: (f64, f64),
    opts: &GenerationOptions,
) -> (f64, f64, f64) {
    let span = (window.1 - window.0).max(0.0);
    let sounding = span * seg.stretch.unwrap_or(1.0);
    if let Some(length) = seg.length {
        return (span, sounding, length);
    }
    let total = (sounding + seg.pause()) / seg.speed.unwrap_or(opts.speed);
    if seg.subword {
        return (span, sounding, total);
    }
    (span, sounding, round_even(total * FPS) / FPS)
}

/// When one word is on screen in the finished video.
#[derive(Debug, Clone, PartialEq)]
pub struct WordSpan {
    pub i: usize,
    pub start: f64,
    pub end: f64,
}

impl WordSpan {
    pub fn to_json(&self) -> Value {
        json!({ "i": self.i, "start": self.start, "end": self.end })
    }
}

/// `[{i, start, end}]`: *i* is the word's index in the report's tokens.
pub fn timeline(
    segments: &[Segment],
    opts: &GenerationOptions,
    audio: &AudioCache,
) -> Vec<WordSpan> {
    let mut spans: std::collections::BTreeMap<usize, [f64; 2]> = Default::default();
    let mut t = 0.0;
    for (k, seg) in segments.iter().enumerate() {
        let pad = if k == segments.len() - 1 { PAD_END_LAST } else { PAD_END };
        let win = extract_window(seg, pad, audio);
        let (_span, sounding, total) = segment_durations(seg, win, opts);
        if !seg.tokens.is_empty() {
            let st = seg.stretch.unwrap_or(1.0);
            for &(i, a, b) in &seg.tokens {
                let lo = t + (a - win.0).max(0.0) * st / opts.speed;
                let hi = t + sounding.min((b - win.0).max(0.0) * st) / opts.speed;
                let s = spans.entry(i).or_insert([lo, hi]);
                s[0] = s[0].min(lo);
                s[1] = s[1].max(hi);
            }
        } else if let Some(i) = seg.tok {
            let s = spans.entry(i).or_insert([t, t]);
            s[1] = t + sounding / opts.speed;
        }
        t += total;
    }
    spans
        .into_iter()
        .map(|(i, [a, b])| WordSpan {
            i,
            start: round3(a),
            end: round3(b),
        })
        .collect()
}

/// Total length of the finished video in seconds.
pub fn total_duration(segments: &[Segment], opts: &GenerationOptions, audio: &AudioCache) -> f64 {
    let mut t = 0.0;
    for (k, seg) in segments.iter().enumerate() {
        let pad = if k == segments.len() - 1 { PAD_END_LAST } else { PAD_END };
        let win = extract_window(seg, pad, audio);
        t += segment_durations(seg, win, opts).2;
    }
    t
}
