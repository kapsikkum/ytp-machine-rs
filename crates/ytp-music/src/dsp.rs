//! Filters and small signal tools (port of `app/ytpmv/dsp.py`).
//!
//! K-weighting (ITU-R BS.1770) as cascaded Direct Form II biquads, zero-crossing
//! snapping, Butterworth low/high-pass, 4x downsampling, edge fades, Haas
//! widening and cepstral spectral-envelope liftering.

use std::f64::consts::PI;

use crate::np::{self, C64};

/// Direct Form II biquad. Coefficients are normalised by `a[0]`.
///
/// Like the Python (numba) core, the state is flushed to zero when it decays
/// below 1e-30 so a dying tail never reaches denormals.
pub fn biquad_df2(x: &[f64], b: [f64; 3], a: [f64; 3]) -> Vec<f64> {
    if x.is_empty() {
        return Vec::new();
    }
    let a0 = a[0];
    let (b0, b1, b2) = (b[0] / a0, b[1] / a0, b[2] / a0);
    let (a1, a2) = (a[1] / a0, a[2] / a0);
    let mut y = Vec::with_capacity(x.len());
    let (mut w1, mut w2) = (0.0f64, 0.0f64);
    for &xi in x {
        let mut w = xi - a1 * w1 - a2 * w2;
        y.push(b0 * w + b1 * w1 + b2 * w2);
        if -1e-30 < w && w < 1e-30 {
            w = 0.0;
        }
        w2 = w1;
        w1 = w;
    }
    y
}

/// `biquad_df2` for float32 audio: filtered in float64, returned as float32.
pub fn biquad_df2_f32(x: &[f32], b: [f64; 3], a: [f64; 3]) -> Vec<f32> {
    let xd: Vec<f64> = x.iter().map(|v| *v as f64).collect();
    biquad_df2(&xd, b, a).into_iter().map(|v| v as f32).collect()
}

/// BS.1770 stage coefficients `(b, a)` for the pre-filter shelf and the RLB high-pass.
pub fn k_weight_coeffs(sr: f64) -> (([f64; 3], [f64; 3]), ([f64; 3], [f64; 3])) {
    let (f0, gain, q) = (1681.974450955533f64, 3.999843853973347f64, 0.7071752369554196f64);
    let a_ = 10.0f64.powf(gain / 40.0);
    let w0 = 2.0 * PI * f0 / sr;
    let alpha = w0.sin() / (2.0 * q);
    let (cw, rt) = (w0.cos(), 2.0 * a_.sqrt() * alpha);
    let b1 = [
        a_ * ((a_ + 1.0) + (a_ - 1.0) * cw + rt),
        -2.0 * a_ * ((a_ - 1.0) + (a_ + 1.0) * cw),
        a_ * ((a_ + 1.0) + (a_ - 1.0) * cw - rt),
    ];
    let a1 = [
        (a_ + 1.0) - (a_ - 1.0) * cw + rt,
        2.0 * ((a_ - 1.0) - (a_ + 1.0) * cw),
        (a_ + 1.0) - (a_ - 1.0) * cw - rt,
    ];

    let (f0, q) = (38.13547087602444f64, 0.5003270373238773f64);
    let w0 = 2.0 * PI * f0 / sr;
    let alpha = w0.sin() / (2.0 * q);
    let cw = w0.cos();
    let b2 = [(1.0 + cw) / 2.0, -(1.0 + cw), (1.0 + cw) / 2.0];
    let a2 = [1.0 + alpha, -2.0 * cw, 1.0 - alpha];
    ((b1, a1), (b2, a2))
}

/// ITU-R BS.1770 K-weighting in the time domain.
pub fn biquad_k_weight(x: &[f64], sr: u32) -> Vec<f64> {
    if x.len() < 2 {
        return x.to_vec();
    }
    let ((b1, a1), (b2, a2)) = k_weight_coeffs(sr as f64);
    let y1 = biquad_df2(x, b1, a1);
    biquad_df2(&y1, b2, a2)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SnapDirection {
    Nearest,
    Left,
    Right,
}

impl SnapDirection {
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "left" | "before" | "backward" => SnapDirection::Left,
            "right" | "after" | "forward" => SnapDirection::Right,
            _ => SnapDirection::Nearest,
        }
    }
}

/// Closest zero crossing with matching slope around `sample_idx`.
pub fn snap_to_zero_crossing(audio: &[f32], sample_idx: i64, window: usize, direction: SnapDirection) -> usize {
    let n = audio.len();
    if n < 2 {
        return sample_idx.max(0) as usize;
    }
    let target = sample_idx.clamp(0, n as i64 - 1) as usize;
    let a = |i: usize| audio[i] as f64;
    let local_slope = if target < n - 1 && target > 0 {
        a(target + 1) - a(target - 1)
    } else if target < n - 1 {
        a(target + 1) - a(target)
    } else {
        a(target) - a(target - 1)
    };
    let target_sign = if local_slope > 0.0 { 1 } else if local_slope < 0.0 { -1 } else { 0 };

    let (lo, hi) = match direction {
        SnapDirection::Left => (target.saturating_sub(window), target),
        SnapDirection::Right => (target, (n - 1).min(target + window)),
        SnapDirection::Nearest => (target.saturating_sub(window), (n - 1).min(target + window)),
    };
    if hi <= lo {
        return target;
    }
    let mut best_match: Option<usize> = None;
    let mut best_match_dist = usize::MAX;
    let mut best_any: Option<usize> = None;
    let mut best_any_dist = usize::MAX;
    for i in lo..hi {
        let (s0, s1) = (a(i), a(i + 1));
        if s0 == 0.0 && s1 == 0.0 {
            continue;
        }
        if (s0 <= 0.0 && s1 >= 0.0) || (s0 >= 0.0 && s1 <= 0.0) {
            let cand = if s0.abs() <= s1.abs() { i } else { i + 1 };
            if direction == SnapDirection::Left && cand > target {
                continue;
            }
            if direction == SnapDirection::Right && cand < target {
                continue;
            }
            let dist = cand.abs_diff(target);
            let cs = s1 - s0;
            let csign = if cs > 0.0 { 1 } else if cs < 0.0 { -1 } else { 0 };
            if dist < best_any_dist {
                best_any_dist = dist;
                best_any = Some(cand);
            }
            if (target_sign == 0 || csign == target_sign) && dist < best_match_dist {
                best_match_dist = dist;
                best_match = Some(cand);
            }
        }
    }
    best_match.or(best_any).unwrap_or(target)
}

fn lowpass_coeffs(cutoff_hz: f64, sr: f64) -> ([f64; 3], [f64; 3]) {
    let w0 = 2.0 * PI * cutoff_hz / sr;
    let alpha = w0.sin() / (2.0 * 2.0f64.sqrt());
    let cw = w0.cos();
    (
        [(1.0 - cw) / 2.0, 1.0 - cw, (1.0 - cw) / 2.0],
        [1.0 + alpha, -2.0 * cw, 1.0 - alpha],
    )
}

fn highpass_coeffs(cutoff_hz: f64, sr: f64) -> ([f64; 3], [f64; 3]) {
    let w0 = 2.0 * PI * cutoff_hz / sr;
    let alpha = w0.sin() / (2.0 * 2.0f64.sqrt());
    let cw = w0.cos();
    (
        [(1.0 + cw) / 2.0, -(1.0 + cw), (1.0 + cw) / 2.0],
        [1.0 + alpha, -2.0 * cw, 1.0 - alpha],
    )
}

/// 2nd-order Butterworth low-pass.
pub fn lowpass_iir(x: &[f64], cutoff_hz: f64, sr: u32) -> Vec<f64> {
    if x.len() < 2 || cutoff_hz <= 0.0 || cutoff_hz >= sr as f64 / 2.0 {
        return x.to_vec();
    }
    let (b, a) = lowpass_coeffs(cutoff_hz, sr as f64);
    biquad_df2(x, b, a)
}

/// 2nd-order Butterworth high-pass.
pub fn highpass_iir(x: &[f64], cutoff_hz: f64, sr: u32) -> Vec<f64> {
    if x.len() < 2 || cutoff_hz <= 0.0 || cutoff_hz >= sr as f64 / 2.0 {
        return x.to_vec();
    }
    let (b, a) = highpass_coeffs(cutoff_hz, sr as f64);
    biquad_df2(x, b, a)
}

/// Downsample by 4 with a 0.2*44100 Hz anti-alias low-pass.
pub fn downsample_4x(x: &[f64]) -> Vec<f64> {
    if x.len() < 8 {
        return x.iter().step_by(4).copied().collect();
    }
    let filtered = lowpass_iir(x, 0.2 * 44100.0, 44100);
    filtered.iter().step_by(4).copied().collect()
}

/// Linear fade-in and fade-out on a mono float32 buffer.
pub fn fade_edge(x: &[f32], fade_ms: f64, sr: u32) -> Vec<f32> {
    let mut out = x.to_vec();
    if x.len() < 2 || fade_ms <= 0.0 {
        return out;
    }
    let fade_len = (np::round_i64(fade_ms * 0.001 * sr as f64).max(0) as usize).min(x.len() / 2);
    if fade_len == 0 {
        return out;
    }
    let ramp = np::linspace_f32(0.0, 1.0, fade_len);
    for i in 0..fade_len {
        out[i] *= ramp[i];
        let k = out.len() - fade_len + i;
        out[k] *= ramp[fade_len - 1 - i];
    }
    out
}

/// Haas effect on a mono signal: left is the signal, right is delayed.
/// Returns interleaved stereo.
pub fn haas_widen_mono(audio: &[f32], delay_ms: f64, sr: u32) -> Vec<f32> {
    let delay = np::round_i64(delay_ms * 0.001 * sr as f64).max(0) as usize;
    let mut out = vec![0.0f32; audio.len() * 2];
    for (i, v) in audio.iter().enumerate() {
        out[2 * i] = *v;
    }
    if delay == 0 {
        for (i, v) in audio.iter().enumerate() {
            out[2 * i + 1] = *v;
        }
        return out;
    }
    if audio.len() > delay {
        for i in delay..audio.len() {
            out[2 * i + 1] = audio[i - delay];
        }
    }
    out
}

/// Haas delay on one side of an interleaved stereo buffer.
pub fn haas_widen_stereo(audio: &[f32], delay_ms: f64, sr: u32, right: bool) -> Vec<f32> {
    let delay = np::round_i64(delay_ms * 0.001 * sr as f64).max(0) as usize;
    let mut out = audio.to_vec();
    let frames = audio.len() / 2;
    if delay == 0 || frames <= delay {
        return out;
    }
    let ch = if right { 1 } else { 0 };
    for i in (delay..frames).rev() {
        out[2 * i + ch] = audio[2 * (i - delay) + ch];
    }
    for i in 0..delay {
        out[2 * i + ch] = 0.0;
    }
    out
}

/// Smooth spectral envelope by cepstral liftering.
pub fn spectral_envelope_lifter(g: &[f64], n_fft: usize, cutoff: usize) -> Vec<f64> {
    let want = (g.len().max(64) as f64).log2().ceil() as u32;
    let n = n_fft.max(1usize << want);
    let spec = np::rfft(g, n);
    let log_mag: Vec<C64> = spec.iter().map(|s| C64::new(s.norm().max(1e-7).ln(), 0.0)).collect();
    let cep = np::irfft(&log_mag, n);
    let mut lifted = vec![0.0f64; cep.len()];
    lifted[0] = cep[0];
    let cut = cutoff.min(cep.len() / 2);
    for i in 1..cut {
        lifted[i] = cep[i] * 2.0;
    }
    np::rfft(&lifted, n).iter().map(|c| c.re.exp()).collect()
}
