//! Picking a take: duration plausibility, vote and edge weights, vetoes,
//! noises and idle footage (`pick_clip`, `_find_clip`, `_pick_idle`, ...).

use crate::audio::AudioCache;
use crate::model::Segment;
use crate::pool::Pool;
use crate::prosody::ToneCtx;
use crate::rng::PyRandom;

pub const CLEAN_AFTER: f64 = 0.10;
pub const CLEAN_BEFORE: f64 = 0.06;
pub const CLEAN_AFTER_WEIGHT: f64 = 60.0;
pub const CLEAN_BEFORE_WEIGHT: f64 = 10.0;
pub const VOTE_BASE: f64 = 2.0;
pub const VOTE_FLOOR: f64 = 0.02;
pub const VOTE_CEIL: f64 = 8.0;
pub const VETO_SCORE: i64 = -2;
pub const MIN_PHONE_DUR: f64 = 0.045;
const IDLE_MAX_RMS: f64 = 400.0;
const IDLE_TRIES: usize = 12;

pub fn vote_weight(score: i64) -> f64 {
    if score == 0 {
        return 1.0;
    }
    VOTE_BASE.powf(score as f64).clamp(VOTE_FLOOR, VOTE_CEIL)
}

/// Minimum plausible duration of a fully articulated `word`.
pub fn duration_floor(word: &str) -> f64 {
    match ytp_text::word_to_phonemes(word) {
        Some(p) if !p.is_empty() => p.len() as f64 * MIN_PHONE_DUR,
        _ => (0.08f64).max(ytp_text::clen(word) as f64 * 0.025),
    }
}

/// How much cleaner this take's edges are likely to cut, as a multiplier.
pub fn edge_weight(c: &crate::model::Clip) -> f64 {
    let mut w = 1.0;
    if c.next_start.is_none_or(|n| n - c.end_time >= CLEAN_AFTER) {
        w *= CLEAN_AFTER_WEIGHT;
    }
    if c.prev_end.is_none_or(|p| c.start_time - p >= CLEAN_BEFORE) {
        w *= CLEAN_BEFORE_WEIGHT;
    }
    w
}

/// A clip voted down far enough to stop being used as a real recording.
pub fn vetoed(pool: &Pool, word: &str, clip_idx: usize) -> bool {
    match pool.clips[clip_idx].id {
        None => false,
        Some(id) => pool.vote(word, id) <= VETO_SCORE,
    }
}

/// Choose a clip of `word` among `rows` (indices into the pool) whose stored
/// duration is plausible, weighted by source quality, edges, votes, reports
/// and tone.
pub fn pick_clip(
    pool: &Pool,
    tone: Option<&ToneCtx>,
    rng: &mut PyRandom,
    rows: &[usize],
    word: &str,
    clean: bool,
) -> Option<usize> {
    if rows.is_empty() {
        return None;
    }
    if rows.len() == 1 {
        return Some(rows[0]);
    }
    let dur = |i: usize| pool.clips[i].duration();
    let mut durs: Vec<f64> = rows.iter().map(|&i| dur(i)).collect();
    durs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = durs[durs.len() / 2];

    let floor = duration_floor(word).max(0.5 * median);
    let ceiling = (2.5 * median).max(0.45);
    let good: Vec<usize> = rows
        .iter()
        .copied()
        .filter(|&i| floor <= dur(i) && dur(i) <= ceiling)
        .collect();
    if good.is_empty() {
        // Nothing in band: the clip closest to the median length.
        return rows
            .iter()
            .copied()
            .min_by(|&a, &b| {
                (dur(a) - median)
                    .abs()
                    .partial_cmp(&(dur(b) - median).abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
    }
    let weights: Vec<f64> = good
        .iter()
        .map(|&i| {
            let c = &pool.clips[i];
            let mut w = if pool.source_quality.is_empty() {
                1.0
            } else {
                pool.source_quality.get(&c.source_id).copied().unwrap_or(1.0).powf(3.0)
            };
            if c.bleed_risk {
                w *= 0.05;
            }
            if clean {
                w *= edge_weight(c);
            }
            if let Some(id) = c.id {
                w *= vote_weight(pool.vote(word, id));
                w *= 0.2f64.powf(pool.boundary_reports.get(&id).copied().unwrap_or(0) as f64);
            }
            if let Some(t) = tone {
                w *= t.weight(c);
            }
            w.max(1e-6)
        })
        .collect();
    if weights.iter().sum::<f64>() > 0.0 {
        Some(good[rng.choices_index(&weights)])
    } else {
        Some(good[rng.choice_index(good.len())])
    }
}

/// `_find_clip`: a take of `word` that has not been voted out.
pub fn find_clip(
    pool: &Pool,
    tone: Option<&ToneCtx>,
    rng: &mut PyRandom,
    word: &str,
    clean: bool,
) -> Option<usize> {
    let rows = pool.by_word.get(word).cloned().unwrap_or_default();
    let usable: Vec<usize> = rows.iter().copied().filter(|&i| !vetoed(pool, word, i)).collect();
    if !rows.is_empty() && usable.is_empty() {
        return None;
    }
    pick_clip(pool, tone, rng, &usable, word, clean)
}

/// A non-verbal noise clip for `noise` (any) or a kind like `click`.
pub fn find_noise(
    pool: &Pool,
    tone: Option<&ToneCtx>,
    rng: &mut PyRandom,
    word: &str,
) -> Option<usize> {
    if word == "noise" {
        return pick_clip(pool, tone, rng, &pool.noises, word, true);
    }
    let rows = pool.noise_by_word.get(word)?;
    pick_clip(pool, tone, rng, rows, word, true)
}

/// A `duration`-second slice of on-screen silence, if the corpus has some.
pub fn pick_idle(
    pool: &Pool,
    audio: &AudioCache,
    rng: &mut PyRandom,
    duration: f64,
) -> Option<Segment> {
    if pool.idle.is_empty() {
        return None;
    }
    let make = |r: &crate::pool::IdleRegion, st: f64, dur: f64, idle: bool| Segment {
        source_file: r.source_file.clone(),
        start_time: st,
        end_time: st + dur,
        prev_end: Some(st),
        next_start: Some(st + dur),
        fade_in: Some(0.0),
        fade_out: Some(0.0),
        idle,
        ..Default::default()
    };
    let cands: Vec<usize> = (0..pool.idle.len())
        .filter(|&i| pool.idle[i].end - pool.idle[i].start >= duration)
        .collect();
    if !cands.is_empty() {
        let k = IDLE_TRIES.min(cands.len());
        for pick in rng.sample(cands.len(), k) {
            let r = &pool.idle[cands[pick]];
            let span = (r.end - r.start) - duration;
            let st = r.start + if span > 0.0 { rng.uniform(0.0, span) } else { 0.0 };
            if audio.region_rms(&r.source_file, st, duration) <= IDLE_MAX_RMS {
                return Some(make(r, st, duration, false));
            }
        }
        return None;
    }
    let r = pool
        .idle
        .iter()
        .fold(None::<&crate::pool::IdleRegion>, |best, r| match best {
            Some(b) if b.end - b.start >= r.end - r.start => Some(b),
            _ => Some(r),
        })?;
    let duration = duration.min(r.end - r.start);
    if duration < 0.08 {
        return None;
    }
    let span = (r.end - r.start) - duration;
    let st = r.start + if span > 0.0 { rng.uniform(0.0, span) } else { 0.0 };
    Some(make(r, st, duration, true))
}
