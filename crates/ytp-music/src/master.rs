//! Balance the parts in a mix by measuring them, then master the result
//! (port of `app/ytpmv/master.py`).
//!
//! Loudness is ITU-R BS.1770: K-weighted 400 ms blocks every 100 ms with
//! absolute and relative gating. `finish` brings a whole mix to a streaming
//! loudness and runs a lookahead peak limiter over it.
//!
//! Multichannel buffers are interleaved (`channels` samples per frame).

use std::collections::HashMap;

use crate::dsp;
use crate::np;

pub const TARGETS: &[(&str, f64)] = &[
    ("lead", 0.0),
    ("bass", -2.9),
    ("chords", -3.4),
    ("rhythm", -5.1),
    ("drums:kick", -4.0),
    ("drums:snare", -10.7),
    ("drums:hats", -23.1),
    ("drums:toms", -4.0),
    ("drums:cymbals", -25.8),
    ("drums:perc", -26.3),
];
pub const DEFAULT_TARGET: f64 = -5.1;
pub const REFERENCE: f64 = -20.0;
pub const MAX_MOVE: f64 = 12.0;
pub const DEADBAND: f64 = 3.0;
pub const STRENGTH: f64 = 0.6;
pub const DEADBAND_DOWN: f64 = 1.5;
pub const STRENGTH_DOWN: f64 = 1.0;
pub const TARGET_LUFS: f64 = -14.0;
pub const CEILING: f64 = 0.89;
pub const RIDE_WINDOW: f64 = 3.0;
pub const RIDE_ABOVE: f64 = 2.0;
pub const RIDE_STRENGTH: f64 = 0.75;
pub const LEAD_TRUST: f64 = 3.0;
const HOP: f64 = 0.1;
const BLOCK: f64 = 0.4;
const LOOKAHEAD: f64 = 0.005;
const RELEASE: f64 = 0.08;
const SILENCE: f64 = -70.0;
const RELATIVE: f64 = -10.0;

pub fn target_for(role: &str) -> f64 {
    TARGETS.iter().find(|(r, _)| *r == role).map(|(_, v)| *v).unwrap_or(DEFAULT_TARGET)
}

/// *x* filtered the way a loudness meter filters it.
pub fn k_weight(x: &[f64], sr: u32) -> Vec<f64> {
    if x.len() < 64 {
        return x.to_vec();
    }
    dsp::biquad_k_weight(x, sr)
}

fn channel(x: &[f64], channels: usize, c: usize) -> Vec<f64> {
    if channels == 1 {
        x.to_vec()
    } else {
        x.iter().skip(c).step_by(channels).copied().collect()
    }
}

/// Mean square of *x*, k-weighted, in 400 ms blocks every 100 ms
/// (stereo is the two channels' powers added).
pub fn block_power(x: &[f64], channels: usize, sr: u32) -> Vec<f64> {
    let channels = channels.max(1);
    let frames = x.len() / channels;
    let block = (0.4 * sr as f64) as usize;
    let hop = (0.1 * sr as f64) as usize;
    let step = 600 * hop;
    let mut out: Vec<f64> = Vec::new();
    let mut i = 0usize;
    loop {
        let end = (i + step + block).min(frames);
        let seg = &x[(i.min(frames)) * channels..end * channels];
        let seg_frames = seg.len() / channels;
        if seg_frames < 64 || !seg.iter().any(|v| *v != 0.0) {
            out.extend(std::iter::repeat_n(0.0, 1usize.max(step.min(seg_frames) / hop)));
        } else {
            let mut total: Option<Vec<f64>> = None;
            for c in 0..channels {
                let mut y = k_weight(&channel(seg, channels, c), sr);
                if y.len() < block {
                    y.resize(block, 0.0);
                }
                let mut cs = Vec::with_capacity(y.len() + 1);
                cs.push(0.0f64);
                let mut run = 0.0f64;
                for v in &y {
                    run += v * v;
                    cs.push(run);
                }
                let limit = step.min(y.len() + 1 - block);
                let mut pw = Vec::new();
                let mut s = 0usize;
                while s < limit {
                    pw.push((cs[s + block] - cs[s]) / block as f64);
                    s += hop;
                }
                total = Some(match total {
                    None => pw,
                    Some(t) => t.iter().zip(pw.iter()).map(|(a, b)| a + b).collect(),
                });
            }
            out.extend(total.unwrap_or_default());
        }
        i += step;
        if i >= frames.max(1) {
            break;
        }
    }
    out
}

/// How loud *x* is while it is playing, in LUFS. `-inf` for silence.
pub fn loudness(x: &[f64], channels: usize, sr: u32) -> f64 {
    if x.len() / channels.max(1) < (sr / 10) as usize {
        return f64::NEG_INFINITY;
    }
    gated(&block_power(x, channels, sr))
}

/// The gated loudness of `block_power()`'s blocks, in LUFS.
pub fn gated(power: &[f64]) -> f64 {
    if power.is_empty() || !power.iter().any(|p| *p != 0.0) {
        return f64::NEG_INFINITY;
    }
    let lk: Vec<f64> = power.iter().map(|p| -0.691 + 10.0 * p.max(1e-20).log10()).collect();
    let loud: Vec<bool> = lk.iter().map(|l| *l > SILENCE).collect();
    if !loud.iter().any(|b| *b) {
        return f64::NEG_INFINITY;
    }
    let mean_of = |mask: &[bool]| -> f64 {
        let sel: Vec<f64> = power.iter().zip(mask.iter()).filter(|(_, m)| **m).map(|(p, _)| *p).collect();
        np::mean_f64(&sel)
    };
    let gate = -0.691 + 10.0 * mean_of(&loud).log10() + RELATIVE;
    let mut keep: Vec<bool> = (0..power.len()).map(|i| loud[i] && lk[i] > gate).collect();
    if !keep.iter().any(|b| *b) {
        keep = loud;
    }
    -0.691 + 10.0 * mean_of(&keep).log10()
}

/// How far to move a part that measured *measured*, as a factor.
pub fn gain_for(measured: f64, role: &str, trim_db: f64, reference: f64, share: f64) -> f64 {
    if !measured.is_finite() {
        return 10.0f64.powf(trim_db / 20.0);
    }
    let err = reference + target_for(role) + 10.0 * share.log10() - measured;
    let mv = if err < 0.0 {
        -(-err - DEADBAND_DOWN).max(0.0) * STRENGTH_DOWN
    } else {
        (err - DEADBAND).max(0.0) * STRENGTH
    };
    let mv = np::clip(mv, -MAX_MOVE, MAX_MOVE) + trim_db;
    10.0f64.powf(mv / 20.0)
}

/// A gain for each of `block_power()`'s blocks, turning loud sections down.
pub fn ride(power: &[f64]) -> Vec<f64> {
    let l = gated(power);
    if !l.is_finite() {
        return vec![1.0; power.len()];
    }
    let n = power.len();
    let w = ((np::round_i64(RIDE_WINDOW / HOP)).max(1) as usize).min(n).max(1);
    let (left, right) = (w / 2, w - w / 2 - 1);
    let st: Vec<f64> = (0..n)
        .map(|i| {
            let lo = i.saturating_sub(left);
            let hi = (i + right).min(n - 1);
            let mut sum = 0.0f64;
            for p in &power[lo..=hi] {
                sum += p;
            }
            let count = (hi - lo + 1) as f64;
            -0.691 + 10.0 * (sum / count).max(1e-20).log10()
        })
        .collect();
    let playing: Vec<f64> = st.iter().filter(|s| **s > l - 20.0).copied().collect();
    let typical = if playing.is_empty() { l } else { np::median_f64(&playing) };
    st.iter()
        .map(|s| {
            let over = s - (typical + RIDE_ABOVE);
            let cut = np::clip(over.max(0.0) * RIDE_STRENGTH, 0.0, MAX_MOVE);
            10.0f64.powf(-cut / 20.0)
        })
        .collect()
}

/// `ride()`'s block gains as one per sample, for samples start..stop.
pub fn envelope(gains: &[f64], start: usize, stop: usize, sr: u32) -> Vec<f32> {
    let centres: Vec<f64> = (0..gains.len()).map(|i| (i as f64 * HOP + BLOCK / 2.0) * sr as f64).collect();
    let xs: Vec<f64> = (start..stop).map(|i| i as f64).collect();
    np::interp(&xs, &centres, gains).iter().map(|v| *v as f32).collect()
}

/// One part to balance: its role, `block_power()` and any trim the person asked for.
#[derive(Clone, Debug)]
pub struct PartPower {
    pub id: String,
    pub role: String,
    pub power: Vec<f64>,
    pub trim_db: f64,
}

/// A part's measured loudness, the gain to apply and its ride envelope.
#[derive(Clone, Debug)]
pub struct Balance {
    pub lufs: f64,
    pub gain: f64,
    pub ride: Vec<f64>,
}

/// Every part's (loudness, gain, ride) from all of them measured together.
pub fn balance(parts: &[PartPower]) -> HashMap<String, Balance> {
    let rides: HashMap<&str, Vec<f64>> = parts.iter().map(|p| (p.id.as_str(), ride(&p.power))).collect();
    let lufs: HashMap<&str, f64> = parts
        .iter()
        .map(|p| {
            let r = &rides[p.id.as_str()];
            let scaled: Vec<f64> = p.power.iter().zip(r.iter()).map(|(a, b)| a * (b * b)).collect();
            (p.id.as_str(), gated(&scaled))
        })
        .collect();
    let known: Vec<&PartPower> = parts.iter().filter(|p| lufs[p.id.as_str()].is_finite()).collect();
    let mut shares: HashMap<&str, f64> = HashMap::new();
    let mut by_role: Vec<(&str, Vec<&PartPower>)> = Vec::new();
    for p in &known {
        match by_role.iter_mut().find(|(r, _)| *r == p.role) {
            Some((_, g)) => g.push(p),
            None => by_role.push((p.role.as_str(), vec![p])),
        }
    }
    for (_, group) in &by_role {
        let n = group.iter().map(|p| p.power.len()).min().unwrap_or(0);
        let on: Vec<Vec<bool>> = group
            .iter()
            .map(|p| {
                let thr = 10.0f64.powf((lufs[p.id.as_str()] + 0.691 - 20.0) / 10.0);
                p.power[..n].iter().map(|v| *v > thr).collect()
            })
            .collect();
        let count: Vec<usize> = (0..n).map(|i| on.iter().filter(|o| o[i]).count()).collect();
        for (p, mine) in group.iter().zip(on.iter()) {
            let sel: Vec<f64> = (0..n).filter(|i| mine[*i]).map(|i| 1.0 / count[i] as f64).collect();
            shares.insert(p.id.as_str(), if sel.is_empty() { 1.0 } else { np::mean_f64(&sel) });
        }
    }
    let implied: Vec<f64> =
        known.iter().map(|p| lufs[p.id.as_str()] - target_for(&p.role) - 10.0 * shares[p.id.as_str()].log10()).collect();
    let leads: Vec<f64> =
        known.iter().zip(implied.iter()).filter(|(p, _)| p.role == "lead").map(|(_, v)| *v).collect();
    let mut reference = REFERENCE;
    if !known.is_empty() {
        reference = np::median_f64(&implied);
        if !leads.is_empty() {
            reference = np::median_f64(&leads).min(reference + LEAD_TRUST);
        }
    }
    parts
        .iter()
        .map(|p| {
            let l = lufs[p.id.as_str()];
            let g = gain_for(l, &p.role, p.trim_db, reference, shares.get(p.id.as_str()).copied().unwrap_or(1.0));
            (p.id.clone(), Balance { lufs: l, gain: g, ride: rides[p.id.as_str()].clone() })
        })
        .collect()
}

/// Master the mix in place: to `TARGET_LUFS`, then limited under `CEILING`.
/// Returns the loudness it was measured at, or `None` for silence.
pub fn finish(mix: &mut [f64], channels: usize, sr: u32) -> Option<f64> {
    if mix.is_empty() {
        return None;
    }
    let was = gated(&block_power(mix, channels, sr));
    if !was.is_finite() {
        return None;
    }
    let g = 10.0f64.powf((TARGET_LUFS - was) / 20.0);
    for v in mix.iter_mut() {
        *v *= g;
    }
    limit(mix, channels, CEILING, sr);
    Some(was)
}

/// A lookahead peak limiter, in place, on millisecond blocks.
pub fn limit(mix: &mut [f64], channels: usize, ceiling: f64, sr: u32) {
    let channels = channels.max(1);
    let blk = ((sr / 1000) as usize).max(1);
    let n = mix.len() / channels;
    if n == 0 {
        return;
    }
    let nb = n.div_ceil(blk);
    let mut peak = vec![0.0f64; nb];
    for (i, frame) in mix.chunks_exact(channels).enumerate() {
        let m = frame.iter().fold(0.0f64, |a, v| a.max(v.abs()));
        let b = i / blk;
        if m > peak[b] {
            peak[b] = m;
        }
    }
    let gain0: Vec<f64> = peak.iter().map(|p| (ceiling / p.max(1e-12)).min(1.0)).collect();
    if gain0.iter().cloned().fold(f64::INFINITY, f64::min) >= 1.0 {
        return;
    }
    let look = (np::round_i64(LOOKAHEAD * 1000.0)).max(1) as usize;
    let padded: Vec<f64> = (0..nb + 2 * look).map(|i| gain0[i.saturating_sub(look).min(nb - 1)]).collect();
    let mut g: Vec<f64> = (0..nb)
        .map(|i| padded[i..i + 2 * look + 1].iter().cloned().fold(f64::INFINITY, f64::min))
        .collect();
    let rel = (-1.0 / (RELEASE * 1000.0)).exp();
    for i in 1..nb {
        let up = 1.0 - (1.0 - g[i - 1]) * rel;
        if g[i] > up {
            g[i] = up;
        }
    }
    let centres: Vec<f64> = (0..nb).map(|i| (i as f64 + 0.5) * blk as f64).collect();
    let xs: Vec<f64> = (0..n).map(|i| i as f64).collect();
    let gs = np::interp(&xs, &centres, &g);
    for (frame, gv) in mix.chunks_exact_mut(channels).zip(gs.iter()) {
        for v in frame {
            *v *= gv;
        }
    }
}
