//! Turn a spoken word into a drum hit, beatbox-style (port of `app/ytpmv/drums.py`).
//!
//! A kick takes the pop of a "b", a snare the burst of a "k" run into a hiss,
//! a hi-hat a clipped "ts". The parts are found from the sound, not from a
//! transcript: the attack is where the level first jumps, the hiss is where
//! the energy sits highest in the spectrum.

use crate::np::{self, C64};
use crate::pitch::SR;
use crate::util::{self, f32c};

pub const GROUPS: [&str; 6] = ["kick", "snare", "hats", "toms", "cymbals", "perc"];

fn hop() -> usize {
    (0.005 * SR as f64) as usize
}

fn frames_env(x: &[f32]) -> Vec<f32> {
    let hop = hop();
    if x.len() < hop {
        return vec![0.0];
    }
    let n = (x.len() / hop).max(1);
    (0..n)
        .map(|i| {
            let sq: Vec<f32> = x[i * hop..(i + 1) * hop].iter().map(|v| v * v).collect();
            (np::sum_f32(&sq) / hop as f32).sqrt()
        })
        .collect()
}

/// Where the word starts: the first moment it reaches a tenth of its peak.
pub fn onset(x: &[f32]) -> usize {
    let env = frames_env(x);
    let peak = env.iter().cloned().fold(0.0f32, f32::max) as f64;
    if peak <= 0.0 {
        return 0;
    }
    let thr = f32c(0.1 * peak);
    let first = env.iter().position(|e| *e >= thr).unwrap_or(0);
    (first as i64 * hop() as i64 - (0.003 * SR as f64) as i64).max(0) as usize
}

/// Per 5 ms: the share of energy above 4 kHz, and the frame's level.
pub fn hiss_profile(x: &[f32]) -> (Vec<f64>, Vec<f64>) {
    let w = (0.02 * SR as f64) as usize;
    if x.len() < w {
        return (vec![0.0], vec![0.0]);
    }
    let hop = hop();
    let hann = np::hanning(w);
    let freqs = np::rfftfreq(w, 1.0 / SR as f64);
    let mut share = Vec::new();
    let mut level = Vec::new();
    let mut start = 0usize;
    while start < x.len() - w {
        let frame: Vec<f64> = (0..w).map(|j| x[start + j] as f64 * hann[j]).collect();
        let spec: Vec<f64> = np::rfft(&frame, w).iter().map(|c| c.norm()).map(|m| m.powf(2.0)).collect();
        let total = np::sum_f64(&spec) + 1e-12;
        let hi: Vec<f64> = spec.iter().zip(freqs.iter()).filter(|(_, f)| **f >= 4000.0).map(|(s, _)| *s).collect();
        share.push(np::sum_f64(&hi) / total);
        level.push(total.sqrt());
        start += hop;
    }
    (share, level)
}

/// The stretch of *x* that is most "s" — start and end sample — if any.
pub fn find_hiss(x: &[f32], min_share: f64) -> Option<(usize, usize)> {
    let (share, level) = hiss_profile(x);
    if share.len() < 2 {
        return None;
    }
    let lmax = level.iter().cloned().fold(0.0f64, f64::max);
    let score: Vec<f64> = share.iter().zip(level.iter()).map(|(s, l)| if *l > 0.05 * lmax { *s } else { 0.0 }).collect();
    let mut best = 0;
    for (i, v) in score.iter().enumerate() {
        if *v > score[best] {
            best = i;
        }
    }
    if score[best] < min_share {
        return None;
    }
    let (mut a, mut b) = (best, best);
    while a > 0 && score[a - 1] >= 0.6 * score[best] {
        a -= 1;
    }
    while b + 1 < score.len() && score[b + 1] >= 0.6 * score[best] {
        b += 1;
    }
    Some((a * hop(), x.len().min((b + 4) * hop())))
}

/// Band-limit *y* with gentle (half-octave) edges. *shelf* is (corner, gain).
fn filter(y: &[f32], lo: Option<f64>, hi: Option<f64>, shelf: Option<(f64, f64)>) -> Vec<f32> {
    if y.len() < 16 {
        return y.to_vec();
    }
    let n = util::pow2_ceil(y.len() * 2);
    let mut spec = np::rfft_f32(y, n);
    let f = np::rfftfreq(n, 1.0 / SR as f64);
    for (s, fr) in spec.iter_mut().zip(f.iter()) {
        let mut h = 1.0f64;
        if let Some(hi) = hi.filter(|v| *v != 0.0) {
            h *= 1.0 / (1.0 + (fr / hi).powf(4.0));
        }
        if let Some(lo) = lo.filter(|v| *v != 0.0) {
            h *= 1.0 - 1.0 / (1.0 + (fr / lo).powf(4.0));
        }
        if let Some((corner, gain)) = shelf {
            h *= 1.0 + (gain - 1.0) / (1.0 + (fr / corner).powf(2.0));
        }
        *s *= C64::new(h, 0.0);
    }
    np::irfft(&spec, n).iter().take(y.len()).map(|v| *v as f32).collect()
}

/// Round the peaks off, adding harmonics of the low end.
fn saturate(y: &[f32], drive: f64) -> Vec<f32> {
    let d = f32c(drive);
    let norm = drive.tanh();
    y.iter().map(|v| ((*v * d).tanh() as f64 / norm) as f32).collect()
}

fn decay(y: &[f32], tau: f64, attack: f64) -> Vec<f32> {
    let n = y.len();
    let mut env: Vec<f64> = (0..n).map(|i| (-(i as f64 / SR as f64) / tau).exp()).collect();
    let a = n.min((attack * SR as f64) as usize);
    if a > 0 {
        let ramp = np::linspace(0.0, 1.0, a);
        for i in 0..a {
            env[i] *= ramp[i];
        }
    }
    let fo = n.min((0.004 * SR as f64) as usize);
    if fo > 0 {
        let ramp = np::linspace(1.0, 0.0, fo);
        for i in 0..fo {
            env[n - fo + i] *= ramp[i];
        }
    }
    y.iter().zip(env.iter()).map(|(v, e)| (*v as f64 * e) as f32).collect()
}

fn slice(x: &[f32], a: usize, dur: f64) -> Vec<f32> {
    let end = (a + (dur * SR as f64) as usize).min(x.len());
    if a >= x.len() {
        return Vec::new();
    }
    x[a..end].to_vec()
}

fn tape(y: &[f32], semitones: f64) -> Vec<f32> {
    let r = 2.0f64.powf(semitones / 12.0);
    let stop = y.len() as f64 - 1.0;
    let count = if stop <= 0.0 { 0 } else { (stop / r).ceil() as usize };
    if count <= 1 {
        return y.to_vec();
    }
    let yd: Vec<f64> = y.iter().map(|v| *v as f64).collect();
    (0..count).map(|i| np::interp_arange_at(i as f64 * r, &yd) as f32).collect()
}

fn match_peak(y: Vec<f32>, reference: &[f32]) -> Vec<f32> {
    let py = np::abs_max_f32(&y) as f64;
    let pr = np::abs_max_f32(reference) as f64;
    if py > 0.0 && pr > 0.0 {
        let k = f32c(pr / py);
        y.iter().map(|v| *v * k).collect()
    } else {
        y
    }
}

/// (the hit, where in *x* it starts — for lining the picture up).
pub fn shape(x: &[f32], group: &str) -> (Vec<f32>, usize) {
    let s = onset(x);
    let hiss = find_hiss(x, 0.35);
    let (y, start): (Vec<f32>, usize) = match group {
        "kick" => {
            let y = tape(&slice(x, s, 0.24), -7.0);
            let sub: Vec<f32> = tape(&y, -12.0).iter().map(|v| *v * f32c(0.55)).collect();
            let mut body = vec![0.0f32; y.len().max(sub.len())];
            for (b, v) in body.iter_mut().zip(y.iter()) {
                *b += v;
            }
            for (b, v) in body.iter_mut().zip(sub.iter()) {
                *b += v;
            }
            let body = filter(&body, None, Some(750.0), Some((90.0, 2.2)));
            let body = saturate(&body, 2.0);
            let mut y = decay(&body, 0.075, 0.002);
            y.truncate((0.2 * SR as f64) as usize);
            (y, s)
        }
        "snare" => {
            let head = slice(x, s, 0.045);
            let tail = match hiss {
                Some((a, _)) if a > s + (0.02 * SR as f64) as usize => slice(x, a, 0.10),
                _ => slice(x, s + head.len(), 0.09),
            };
            let xf = head.len().min(tail.len()).min((0.006 * SR as f64) as usize);
            let y = if xf > 0 {
                let ramp = np::linspace_f32(0.0, 1.0, xf);
                let mut y: Vec<f32> = head[..head.len() - xf].to_vec();
                for i in 0..xf {
                    y.push(head[head.len() - xf + i] * (1.0f32 - ramp[i]) + tail[i] * ramp[i]);
                }
                y.extend_from_slice(&tail[xf..]);
                y
            } else {
                let mut y = head;
                y.extend_from_slice(&tail);
                y
            };
            let y = filter(&y, Some(250.0), None, None);
            (decay(&y, 0.05, 0.002), s)
        }
        "hats" => {
            let a = hiss.map(|h| h.0).unwrap_or(s);
            let y = filter(&slice(x, a, 0.07), Some(3500.0), None, None);
            (decay(&y, 0.022, 0.002), a)
        }
        "cymbals" => {
            let (a, y) = match hiss {
                Some((a, b)) => {
                    let dur = 0.6f64.min(0.25f64.max((b - a) as f64 / SR as f64 + 0.1));
                    (a, slice(x, a, dur))
                }
                None => (s, slice(x, s, 0.35)),
            };
            let y = filter(&y, Some(2500.0), None, None);
            (decay(&y, 0.18, 0.004), a)
        }
        "toms" => {
            let y = filter(&slice(x, s, 0.22), None, Some(2500.0), None);
            (decay(&y, 0.08, 0.002), s)
        }
        _ => (decay(&slice(x, s, 0.08), 0.025, 0.002), s),
    };
    if y.is_empty() {
        return (x[..x.len().min((0.05 * SR as f64) as usize)].to_vec(), 0);
    }
    (match_peak(y, x), start)
}
