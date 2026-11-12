//! Held letters (`loooong`), boomerangs and chaos: segment surgery that the
//! encoder already knows how to play (`_hold`, `_stretch_word`, `_sprinkle`
//! in `app/generate.py`).

use ytp_text::phonemes::word_to_phonemes;
use ytp_text::{is_vowel, tokenize::StretchMark};

use crate::align::char_times_by_id;
use crate::model::Segment;
use crate::pool::Pool;
use crate::rng::PyRandom;

pub const MAX_STRETCH_ADD: f64 = 3.0;
pub const MAX_STRETCH: f64 = 6.0;

/// The word forwards, backwards, forwards -- the SUS-meme boomerang.
pub fn boom_copies(seg: &Segment) -> Vec<Segment> {
    (0..3)
        .map(|k| {
            let mut c = seg.clone();
            if k == 1 {
                c.reverse = !seg.reverse;
            }
            if k != 2 {
                c.pause_after = Some(0.0);
            }
            c
        })
        .collect()
}

/// `seg` split so that source seconds a..b play `factor` times longer.
pub fn hold(seg: &Segment, a: f64, b: f64, factor: f64) -> Vec<Segment> {
    let a = seg.start_time.max(a);
    let b = seg.end_time.min(b);
    if b - a < 0.02 || factor <= 1.0 {
        return vec![seg.clone()];
    }
    let mut pieces: Vec<Segment> = Vec::new();
    if a - seg.start_time >= 0.01 {
        let mut head = seg.clone();
        head.end_time = a;
        head.next_start = Some(a);
        head.fade_out = Some(0.0);
        head.pause_after = Some(0.0);
        head.stretch = None;
        pieces.push(head);
    }
    let mut mid = seg.clone();
    mid.start_time = a;
    mid.end_time = b;
    mid.prev_end = Some(a);
    mid.next_start = Some(b);
    mid.fade_in = if pieces.is_empty() { seg.fade_in } else { Some(0.0) };
    mid.fade_out = Some(0.0);
    mid.pause_after = Some(0.0);
    mid.stretch = Some(factor);
    mid.subword = true;
    let has_tail = seg.end_time - b >= 0.01;
    if !has_tail {
        mid.fade_out = seg.fade_out;
        mid.pause_after = Some(seg.pause());
        mid.next_start = seg.next_start;
    }
    pieces.push(mid);
    if has_tail {
        let mut tail = seg.clone();
        tail.start_time = b;
        tail.prev_end = Some(b);
        tail.fade_in = Some(0.0);
        tail.stretch = None;
        pieces.push(tail);
    }
    pieces
}

/// Where letters pos..pos+keep of `word` are sounded inside a whole-word clip.
fn letter_span(pool: &Pool, seg: &Segment, word: &str, pos: usize, keep: usize) -> (f64, f64) {
    let letters = word.chars().filter(|c| c.is_alphabetic()).count();
    if let Some(ct) = char_times_by_id(pool, seg.id) {
        if ct.len() == letters && pos + keep <= ct.len() {
            let a = seg.start_time + ct[pos].1;
            let b = if pos + keep < ct.len() {
                seg.start_time + ct[pos + keep].1
            } else {
                seg.end_time
            };
            if b > a {
                return (a, b);
            }
        }
    }
    let d = seg.end_time - seg.start_time;
    let n = letters.max(1) as f64;
    (
        seg.start_time + d * pos as f64 / n,
        seg.start_time + d * (pos + keep) as f64 / n,
    )
}

struct Item {
    uid: usize,
    seg: Segment,
}

/// The segments that say `word`, with each marked letter held longer.
pub fn stretch_word(
    pool: &Pool,
    segs: Vec<Segment>,
    word: &str,
    marks: &[StretchMark],
    per_letter: f64,
) -> Vec<Segment> {
    if segs.is_empty() || marks.is_empty() {
        return segs;
    }
    let mut items: Vec<Item> = segs
        .into_iter()
        .enumerate()
        .map(|(uid, seg)| Item { uid, seg })
        .collect();
    let mut next_uid = items.len();
    let original_count = items.len();
    let wchars: Vec<char> = word.chars().collect();

    // (a, b, extra, source file, owner uid)
    let mut holds: Vec<(f64, f64, usize, String, usize)> = Vec::new();
    for &(pos, keep, extra) in marks {
        let (a, b, owner);
        if items.len() == 1 && items[0].seg.unit.is_none() {
            let seg = &items[0].seg;
            let (la, lb) = letter_span(pool, seg, word, pos, keep);
            (a, b, owner) = (la, lb, 0);
        } else {
            let phones = word_to_phonemes(word).unwrap_or_default();
            if phones.is_empty() {
                continue;
            }
            let idx = (phones.len() - 1).min(pos * phones.len() / wchars.len().max(1));
            let vowel = wchars.get(pos).is_some_and(|c| "aeiouy".contains(*c));
            let target = (0..phones.len())
                .min_by_key(|&j| (is_vowel(&phones[j]) != vowel, j.abs_diff(idx)))
                .unwrap();
            let found = items.iter().position(|it| {
                it.seg
                    .unit
                    .as_ref()
                    .is_some_and(|u| u.at <= target && target < u.at + u.phones.len())
            });
            match found {
                None => {
                    let k = items
                        .iter()
                        .enumerate()
                        .fold(0, |best, (k, it)| {
                            let d = |i: usize| items[i].seg.end_time - items[i].seg.start_time;
                            if (it.seg.end_time - it.seg.start_time) > d(best) {
                                k
                            } else {
                                best
                            }
                        });
                    let seg = &items[k].seg;
                    let d = seg.end_time - seg.start_time;
                    (a, b, owner) = (seg.start_time + 0.25 * d, seg.start_time + 0.75 * d, k);
                }
                Some(k) => {
                    let seg = &items[k].seg;
                    let u = seg.unit.as_ref().unwrap();
                    let d = (seg.end_time - seg.start_time) / u.phones.len() as f64;
                    let j = (target - u.at) as f64;
                    (a, b, owner) = (seg.start_time + d * j, seg.start_time + d * (j + 1.0), k);
                }
            }
        }
        let seg = &items[owner].seg;
        let (mut a, mut b) = (a, b);
        if b - a < 0.03 {
            let mid = (a + b) / 2.0;
            a = seg.start_time.max(mid - 0.015);
            b = seg.end_time.min(a + 0.03);
        }
        holds.push((a, b, extra, seg.source_file.clone(), items[owner].uid));
    }

    let mut added = 0.0;
    holds.sort_by(|x, y| y.0.partial_cmp(&x.0).unwrap_or(std::cmp::Ordering::Equal));
    for (mut a, mut b, extra, src, owner) in holds {
        let want = (extra as f64 * per_letter).min(MAX_STRETCH_ADD - added);
        if want <= 0.01 {
            continue;
        }
        let mid = (a + b) / 2.0;
        let covers = |s: &Segment| s.start_time <= mid && mid < s.end_time;
        let mut at = if owner < original_count {
            items.iter().position(|it| it.uid == owner)
        } else {
            None
        };
        if at.is_none_or(|k| !covers(&items[k].seg)) {
            at = items.iter().position(|it| {
                it.seg.source_file == src
                    && it.seg.stretch.is_none_or(|s| s == 0.0)
                    && covers(&it.seg)
            });
        }
        let Some(at) = at else { continue };
        let piece = &items[at].seg;
        let mut span = b - a;
        if (span + want) / span > MAX_STRETCH {
            let wider = want / (MAX_STRETCH - 1.0);
            let grow = (wider - span) / 2.0;
            a = piece.start_time.max(a - grow);
            b = piece.end_time.min(b + grow);
            span = b - a;
        }
        let factor = MAX_STRETCH.min((span + want) / span);
        let pieces = hold(piece, a, b, factor);
        let new_items: Vec<Item> = pieces
            .into_iter()
            .map(|seg| {
                next_uid += 1;
                Item { uid: next_uid - 1, seg }
            })
            .collect();
        items.splice(at..=at, new_items);
        added += span * (factor - 1.0);
    }
    items.into_iter().map(|it| it.seg).collect()
}

/// Wreck words at random, the way a YTP is actually edited. `names` are the
/// glitch effects the encoder can actually do.
pub fn sprinkle(segments: &mut Vec<Segment>, chaos: f64, names: &[String], rng: &mut PyRandom) {
    let mut out: Vec<Segment> = Vec::new();
    for mut sg in std::mem::take(segments) {
        if !sg.is_spoken() || rng.random() >= chaos {
            out.push(sg);
            continue;
        }
        if !names.is_empty() {
            let k = if rng.random() < 0.7 { 1 } else { 2.min(names.len()) };
            sg.glitch = rng.sample(names.len(), k).into_iter().map(|i| names[i].clone()).collect();
        }
        let r = rng.random();
        if r < 0.12 {
            sg.reverse = !sg.reverse;
        } else if r < 0.30 {
            const P: [f64; 4] = [-7.0, -5.0, 5.0, 7.0];
            // random.choice((-7, -5, 5, 7, 12))
            let all = [P[0], P[1], P[2], P[3], 12.0];
            sg.pitch = Some(sg.pitch.unwrap_or(0.0) + all[rng.choice_index(5)]);
        } else if r < 0.44 {
            let f = [0.6, 1.8][rng.choice_index(2)];
            sg.stretch = Some(sg.stretch.filter(|s| *s != 0.0).unwrap_or(1.0) * f);
        } else if r < 0.52 {
            out.extend(boom_copies(&sg));
            continue;
        }
        out.push(sg);
    }
    *segments = out;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(a: f64, b: f64) -> Segment {
        Segment {
            word: "loong".into(),
            start_time: a,
            end_time: b,
            source_file: "x.mp4".into(),
            prev_end: Some(a - 0.1),
            next_start: Some(b + 0.1),
            ..Default::default()
        }
    }

    #[test]
    fn hold_splits_into_head_mid_tail() {
        let p = hold(&seg(1.0, 2.0), 1.3, 1.5, 4.0);
        assert_eq!(p.len(), 3);
        assert_eq!(p[0].end_time, 1.3);
        assert_eq!(p[0].stretch, None);
        assert_eq!(p[1].stretch, Some(4.0));
        assert!(p[1].subword);
        assert_eq!(p[2].start_time, 1.5);
        assert_eq!(p[2].fade_in, Some(0.0));
        let whole = hold(&seg(1.0, 2.0), 1.0, 1.01, 4.0);
        assert_eq!(whole.len(), 1);
    }

    #[test]
    fn held_letter_grows_the_word() {
        let pool = Pool::default();
        let out = stretch_word(&pool, vec![seg(1.0, 2.0)], "long", &[(1, 1, 3)], 0.09);
        let total: f64 = out
            .iter()
            .map(|s| (s.end_time - s.start_time) * s.stretch.unwrap_or(1.0))
            .sum();
        assert!(total > 1.0, "{total}");
        assert!(out.iter().any(|s| s.stretch.is_some_and(|f| f > 1.0)));
    }

    #[test]
    fn boom_is_three_copies_middle_reversed() {
        let c = boom_copies(&seg(0.0, 1.0));
        assert_eq!(c.len(), 3);
        assert!(!c[0].reverse && c[1].reverse && !c[2].reverse);
        assert_eq!(c[0].pause_after, Some(0.0));
        assert_eq!(c[2].pause_after, None);
    }
}
