//! Text -> the segments that would say it, without encoding
//! (`_resolve_text` in `app/generate.py`): runs of real speech, single
//! takes, phoneme splices, noises, pacing, effects.

use std::sync::Arc;

use ytp_text::tokenize::{effective_fx, tokenize_full, EffectiveFx};

use crate::audio::AudioCache;
use crate::model::{Report, Segment, TokenReport};
use crate::options::GenerationOptions;
use crate::phoneme_splice::{find_phoneme_splice, SpliceCtx};
use crate::pool::Pool;
use crate::prosody::{target as tone_target, ToneCtx};
use crate::rng::PyRandom;
use crate::select::{find_clip, find_noise, pick_idle, vetoed};
use crate::stretch::{boom_copies, sprinkle, stretch_word};

pub const MAX_RUN_GAP: f64 = 0.60;
pub const BLEED_TAIL: f64 = 0.16;
pub const TAIL_GAP: f64 = 0.05;

/// The generation was cancelled from the progress callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

/// `progress(stage, done, total)`; return `false` to cancel.
pub type Progress<'a> = &'a dyn Fn(&str, usize, usize) -> bool;

/// Everything resolving needs besides the text and the options.
pub struct Runtime<'a> {
    /// The corpus being spoken in.
    pub pool: Arc<Pool>,
    /// Another corpus's voice by name, for `word{v=name}`.
    pub voices: &'a dyn Fn(&str) -> Option<Arc<Pool>>,
    pub audio: &'a AudioCache<'a>,
    /// Glitch effects the encoder can actually do (for chaos).
    pub glitches: Vec<String>,
}

fn find_run(pool: &Pool, words: &[String], i: usize) -> Option<(i64, usize, usize, usize)> {
    let mut best: Option<(i64, usize, usize, usize)> = None;
    let mut best_q = -1.0;
    for &(src, idx) in pool.word_positions.get(&words[i]).into_iter().flatten() {
        let seq = &pool.ordered_by_source[&src];
        if vetoed(pool, &words[i], seq[idx]) {
            continue;
        }
        let mut k = 1;
        while i + k < words.len()
            && idx + k < seq.len()
            && pool.clips[seq[idx + k]].word == words[i + k]
            && pool.clips[seq[idx + k]].start_time - pool.clips[seq[idx + k - 1]].end_time
                <= MAX_RUN_GAP
            && !vetoed(pool, &words[i + k], seq[idx + k])
        {
            k += 1;
        }
        if k < 2 {
            continue;
        }
        let q = pool.source_quality.get(&src).copied().unwrap_or(1.0);
        if best.is_none_or(|b| k > b.3 || (k == b.3 && q > best_q)) {
            best = Some((src, idx, idx + k - 1, k));
            best_q = q;
        }
    }
    best
}

/// One clip spanning source words `s..=e`, ending at the last word's content end.
fn merged_run_clip(pool: &Pool, src: i64, s: usize, e: usize) -> Segment {
    let seq = &pool.ordered_by_source[&src];
    let (first, last) = (&pool.clips[seq[s]], &pool.clips[seq[e]]);
    let mut merged = Segment::from_clip(first);
    merged.word = (s..=e)
        .map(|j| pool.clips[seq[j]].word.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    merged.prev_end = first.prev_end;

    let mut end = last.end_time;
    let real_next = last.next_start;
    if let Some(rel) = crate::align::word_core_end(pool, last) {
        if last.start_time + rel > last.start_time + 0.05 {
            end = last.start_time + rel;
        }
    }
    if let Some(next) = real_next {
        let gap = if last.bleed_risk { BLEED_TAIL } else { TAIL_GAP };
        end = end.min(next - gap);
        end = end.max(last.start_time + 0.05);
    }
    merged.end_time = end;
    merged.next_start = Some(end);
    merged
}

#[allow(clippy::too_many_arguments)]
fn set_pacing(
    segments: &mut Vec<Segment>,
    pool: &Pool,
    rt: &Runtime,
    rng: &mut PyRandom,
    ends: bool,
    last_word: bool,
    stop_pause: f64,
    word_gap: f64,
    idle_ok: bool,
) {
    if ends {
        let idle = if stop_pause >= 0.1 && idle_ok {
            pick_idle(pool, rt.audio, rng, stop_pause)
        } else {
            None
        };
        if let Some(idle) = idle {
            segments.push(idle);
        } else if stop_pause > 0.0 {
            let l = segments.last_mut().unwrap();
            l.pause_after = Some(l.pause().max(stop_pause));
        }
    } else if !last_word && word_gap > 0.0 {
        let l = segments.last_mut().unwrap();
        l.pause_after = Some(l.pause().max(word_gap));
    }
}

/// See the module docs. `rng` must already be seeded.
pub fn resolve_text(
    text: &str,
    opts: &GenerationOptions,
    rt: &Runtime,
    rng: &mut PyRandom,
    progress: Progress,
) -> Result<(Vec<Segment>, Report), Cancelled> {
    let say = |stage: &str, done: usize, total: usize| -> Result<(), Cancelled> {
        if progress(stage, done, total) {
            Ok(())
        } else {
            Err(Cancelled)
        }
    };
    let (word_gap, mut stop_pause) = (opts.word_gap, opts.sentence_pause);
    let pause_style = opts.pause_style.round_ties_even() as i64;
    if pause_style == 2 {
        stop_pause = 0.0;
    }
    let idle_ok = pause_style == 0;

    let own = rt.pool.clone();
    let _pronunciations = own.scope();
    let full = tokenize_full(text, own.as_ref());
    if full.is_empty() {
        return Ok((Vec::new(), Report::default()));
    }
    let words: Vec<String> = full.iter().map(|t| t.word.clone()).collect();
    let ends: Vec<bool> = full.iter().map(|t| t.ends).collect();
    let is_noise: Vec<bool> = full.iter().map(|t| t.noise).collect();
    let is_rev: Vec<bool> = full.iter().map(|t| t.reverse).collect();
    let shown: Vec<String> = full.iter().map(|t| t.shown.clone()).collect();
    let fx: Vec<EffectiveFx> = full.iter().map(|t| effective_fx(&t.fx)).collect();

    say("loading", 0, 1)?;
    say("loading", 1, 1)?;

    let mut report = Report::default();
    let mut segments: Vec<Segment> = Vec::new();

    let mut voice: Option<String> = None;
    let mut pool = own.clone();
    let mut borrowed = false;
    let mut mode = pool.splice_mode().to_string();
    let mut units_cap = pool.max_units();
    let mut tone_ctx = ToneCtx::new(opts.tone_match);
    let n = words.len();
    let mut i = 0;
    while i < n {
        say("resolving", i, n)?;
        if tone_ctx.is_some() && (i == 0 || ends[i - 1]) {
            let mut j = i;
            while j < n && !ends[j] {
                j += 1;
            }
            let cands: Vec<Vec<&crate::model::Clip>> = words[i..(j + 1).min(n)]
                .iter()
                .map(|w| {
                    pool.by_word
                        .get(w)
                        .map(|v| {
                            v.iter()
                                .filter(|&&c| !vetoed(&pool, w, c))
                                .map(|&c| &pool.clips[c])
                                .collect()
                        })
                        .unwrap_or_default()
                })
                .collect();
            let t = tone_target(&cands);
            if let Some(ctx) = tone_ctx.as_mut() {
                ctx.target = t;
            }
        }
        if fx[i].voice != voice {
            voice = fx[i].voice.clone();
            pool = match &voice {
                Some(v) => (rt.voices)(v).unwrap_or_else(|| own.clone()),
                None => own.clone(),
            };
            borrowed = !Arc::ptr_eq(&pool, &own);
            mode = pool.splice_mode().to_string();
            units_cap = pool.max_units();
            if let Some(ctx) = tone_ctx.as_mut() {
                ctx.prev = None;
                ctx.target = None;
            }
        }
        let seg_before = segments.len();
        let tok_before = report.tokens.len();
        let mut used_run = false;
        let mut cap = 1;
        let last_idx;

        // 0) An explicit *noise* token.
        if is_noise[i] {
            if let Some(nz) = find_noise(&pool, tone_ctx.as_ref(), rng, &words[i]) {
                report.found.push(words[i].clone());
                report.tokens.push(TokenReport {
                    word: words[i].clone(),
                    status: "found".into(),
                    ..Default::default()
                });
                let mut seg = Segment::from_clip(&pool.clips[nz]);
                seg.tok = Some(report.tokens.len() - 1);
                segments.push(seg);
                if is_rev[i] {
                    for s in &mut segments[seg_before..] {
                        s.reverse = true;
                    }
                }
                set_pacing(&mut segments, &pool, rt, rng, ends[i], i >= n - 1, stop_pause, word_gap, idle_ok);
                i += 1;
                continue;
            }
        }

        // 1) A contiguous phrase already spoken in some source.
        let run = if opts.phrases && full[i].stretch.is_empty() && fx[i].clip.is_none() {
            find_run(&pool, &words, i)
        } else {
            None
        };
        if let Some((src, s, _e, length)) = run {
            let mut rcap = length;
            for k in 0..length - 1 {
                if ends[i + k]
                    || is_rev[i + k] != is_rev[i + k + 1]
                    || !full[i + k + 1].stretch.is_empty()
                    || fx[i + k + 1] != fx[i]
                {
                    rcap = k + 1;
                    break;
                }
            }
            if rcap >= 2 {
                cap = rcap;
                let seq = &pool.ordered_by_source[&src];
                let mut merged = merged_run_clip(&pool, src, s, s + cap - 1);
                merged.subtitle_words = (0..cap)
                    .map(|k| {
                        let c = &pool.clips[seq[s + k]];
                        (shown[i + k].clone(), c.start_time, c.end_time)
                    })
                    .collect();
                merged.tokens = (0..cap)
                    .map(|k| {
                        let c = &pool.clips[seq[s + k]];
                        (report.tokens.len() + k, c.start_time, c.end_time)
                    })
                    .collect();
                report.found.extend(words[i..i + cap].iter().cloned());
                report.runs.push(merged.word.clone());
                for (k, w) in words[i..i + cap].iter().enumerate() {
                    report.tokens.push(TokenReport {
                        word: w.clone(),
                        status: "run".into(),
                        clips: Some(pool.clips[seq[s + k]].id.into_iter().collect()),
                        ..Default::default()
                    });
                }
                segments.push(merged);
                last_idx = i + cap - 1;
                used_run = true;
                if let Some(ctx) = tone_ctx.as_mut() {
                    let c = &pool.clips[seq[s + cap - 1]];
                    ctx.follow(c.tone.as_ref(), c.source_id);
                }
            } else {
                last_idx = i;
            }
        } else {
            last_idx = i;
        }
        let mut last_idx = last_idx;

        if !used_run {
            cap = 1;
            let word = &words[i];
            let chosen = fx[i].clip;
            let mut clip = chosen.and_then(|want| {
                pool.by_word
                    .get(word)
                    .into_iter()
                    .flatten()
                    .copied()
                    .find(|&c| pool.clips[c].id == Some(want))
            });
            if clip.is_none() {
                clip = find_clip(&pool, tone_ctx.as_ref(), rng, word, opts.clean_takes);
            }
            if let Some(ci) = clip {
                report.found.push(word.clone());
                let c = &pool.clips[ci];
                report.tokens.push(TokenReport {
                    word: word.clone(),
                    status: "found".into(),
                    clips: Some(c.id.into_iter().collect()),
                    ..Default::default()
                });
                let tok = report.tokens.len() - 1;
                let mut seg = Segment::from_clip(c);
                seg.subtitle = Some(shown[i].clone());
                seg.tok = Some(tok);
                if !full[i].stretch.is_empty() {
                    let mut held = stretch_word(&pool, vec![seg], word, &full[i].stretch, opts.stretch);
                    for p in &mut held {
                        p.tok = Some(tok);
                    }
                    report.tokens[tok].stretched = Some(shown[i].clone());
                    segments.extend(held);
                } else {
                    segments.push(seg);
                }
            } else {
                let cx = SpliceCtx {
                    pool: &pool,
                    tone: tone_ctx.as_ref(),
                    audio: rt.audio,
                };
                let penalty = pool.penalties(word);
                match find_phoneme_splice(&cx, rng, word, &penalty, &mode, units_cap) {
                    Some(mut segs) => {
                        report.spliced.push(word.clone());
                        let mut ids: Vec<i64> = segs.iter().filter_map(|s| s.id).collect();
                        ids.sort_unstable();
                        ids.dedup();
                        let approx = segs.iter().any(|s| s.approx);
                        report.tokens.push(TokenReport {
                            word: word.clone(),
                            status: "spliced".into(),
                            clips: Some(ids),
                            approx,
                            ..Default::default()
                        });
                        let tok = report.tokens.len() - 1;
                        for u in &mut segs {
                            u.subtitle = Some(shown[i].clone());
                            u.tok = Some(tok);
                        }
                        if !full[i].stretch.is_empty() {
                            segs = stretch_word(&pool, segs, word, &full[i].stretch, opts.stretch);
                            for u in &mut segs {
                                u.tok = Some(tok);
                            }
                            report.tokens[tok].stretched = Some(shown[i].clone());
                        }
                        segments.extend(segs);
                    }
                    None => {
                        report.missing.push(word.clone());
                        report.tokens.push(TokenReport {
                            word: word.clone(),
                            status: "missing".into(),
                            ..Default::default()
                        });
                    }
                }
            }
            last_idx = i;
            if let Some(ctx) = tone_ctx.as_mut() {
                if let Some(sg) = segments[seg_before..].iter().rev().find(|s| s.tone.is_some()) {
                    ctx.follow(sg.tone.as_ref(), sg.source_id);
                }
            }
        }

        if is_rev[i..i + cap].iter().any(|r| *r) {
            for s in &mut segments[seg_before..] {
                s.reverse = true;
            }
        }

        if segments.len() > seg_before {
            set_pacing(&mut segments, &pool, rt, rng, ends[last_idx], last_idx >= n - 1, stop_pause, word_gap, idle_ok);
        }

        let f = &fx[i];
        if !f.is_empty() && segments.len() > seg_before {
            let own_idx: Vec<usize> = (seg_before..segments.len())
                .filter(|&k| segments[k].is_spoken())
                .collect();
            for &k in &own_idx {
                let sg = &mut segments[k];
                if let Some(rate) = f.rate.filter(|r| *r != 1.0) {
                    sg.stretch = Some(sg.stretch.filter(|s| *s != 0.0).unwrap_or(1.0) / rate);
                }
                if let Some(p) = f.pitch.filter(|p| *p != 0.0) {
                    sg.pitch = Some(p);
                }
                if let Some(v) = f.vol.filter(|v| *v != 0.0) {
                    sg.gain_db = Some(v);
                }
                if !f.glitch.is_empty() {
                    sg.glitch = f.glitch.clone();
                }
            }
            if let (Some(p), Some(&k)) = (f.pause, own_idx.last()) {
                segments[k].pause_after = Some(p);
            }
            if f.boom && !own_idx.is_empty() {
                let tail: Vec<Segment> = segments.split_off(seg_before);
                for (off, sg) in tail.into_iter().enumerate() {
                    if own_idx.contains(&(seg_before + off)) {
                        segments.extend(boom_copies(&sg));
                    } else {
                        segments.push(sg);
                    }
                }
            }
        }

        if borrowed {
            for t in &mut report.tokens[tok_before..] {
                t.voice = Some(pool.slug.clone());
            }
        }
        i += cap;
    }

    if opts.chaos > 0.0 {
        sprinkle(&mut segments, opts.chaos, &rt.glitches, rng);
    }

    if segments.is_empty() {
        let missing = report.missing;
        let tokens = report.tokens;
        return Ok((
            Vec::new(),
            Report {
                missing,
                tokens,
                ..Default::default()
            },
        ));
    }

    say("resolving", n, n)?;
    for (tok, sh) in report.tokens.iter_mut().zip(&shown) {
        if sh.to_lowercase() != tok.word && tok.stretched.is_none() {
            tok.shown = Some(sh.clone());
        }
    }
    Ok((segments, report))
}
