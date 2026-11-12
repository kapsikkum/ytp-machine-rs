//! Small helpers shared by the chip modules.

use crate::np::{self, C64};

/// `1 << ceil(log2(x))`, with Python's `int(np.ceil(np.log2(x)))`.
pub fn pow2_ceil(x: usize) -> usize {
    let l = (x.max(1) as f64).log2().ceil() as u32;
    1usize << l
}

/// How a chip's `_lowpass` picks its FFT length.
#[derive(Clone, Copy)]
pub enum FftLen {
    /// `1 << ceil(log2(len * 2))`
    Double,
    /// `1 << ceil(log2(len + extra))`
    Plus(usize),
}

impl FftLen {
    pub fn of(self, len: usize) -> usize {
        match self {
            FftLen::Double => pow2_ceil(len * 2),
            FftLen::Plus(extra) => pow2_ceil(len + extra),
        }
    }
}

fn lp8_gain(f: f64, hz: f64) -> f64 {
    1.0 / (1.0 + (f / hz).powf(8.0))
}

/// The chips' shared eighth-order-ish anti-alias filter on a float64 signal:
/// `irfft(rfft(y, n) / (1 + (f / hz) ** 8), n)[:len(y)]`.
pub fn lowpass8_f64(y: &[f64], hz: f64, sr: f64, n: FftLen) -> Vec<f64> {
    if y.len() < 32 || hz >= sr / 2.0 {
        return y.to_vec();
    }
    np::fft_filter(y, n.of(y.len()), sr, |f| C64::new(lp8_gain(f, hz), 0.0))
}

/// Same, for float32 input (numpy transforms float32 in single precision).
pub fn lowpass8_f32(y: &[f32], hz: f64, sr: f64, n: FftLen) -> Vec<f64> {
    if y.len() < 32 || hz >= sr / 2.0 {
        return y.iter().map(|v| *v as f64).collect();
    }
    let nfft = n.of(y.len());
    let mut spec = np::rfft_f32(y, nfft);
    let f = np::rfftfreq(nfft, 1.0 / sr);
    for (s, fr) in spec.iter_mut().zip(f.iter()) {
        *s *= lp8_gain(*fr, hz);
    }
    let mut out = np::irfft(&spec, nfft);
    out.truncate(y.len());
    out
}

/// `out[-edge:] *= np.linspace(1.0, 0.0, edge, dtype=np.float32)` on float32.
pub fn fade_tail_f32(out: &mut [f32], edge: usize) {
    if edge > 1 && edge <= out.len() {
        let ramp = np::linspace_f32(1.0, 0.0, edge);
        let start = out.len() - edge;
        for (o, r) in out[start..].iter_mut().zip(ramp.iter()) {
            *o *= *r;
        }
    }
}

/// `out[-edge:] *= np.linspace(1.0, 0.0, edge, dtype=np.float32)` on float64.
pub fn fade_tail_f64_by_f32(out: &mut [f64], edge: usize) {
    if edge > 1 && edge <= out.len() {
        let ramp = np::linspace_f32(1.0, 0.0, edge);
        let start = out.len() - edge;
        for (o, r) in out[start..].iter_mut().zip(ramp.iter()) {
            *o *= *r as f64;
        }
    }
}

/// `out[-edge:] *= np.linspace(1.0, 0.0, edge)` on float64.
pub fn fade_tail_f64(out: &mut [f64], edge: usize) {
    if edge > 1 && edge <= out.len() {
        let ramp = np::linspace(1.0, 0.0, edge);
        let start = out.len() - edge;
        for (o, r) in out[start..].iter_mut().zip(ramp.iter()) {
            *o *= *r;
        }
    }
}

/// Python `float` converted the way a numpy float32 array op sees it.
#[inline]
pub fn f32c(x: f64) -> f32 {
    x as f32
}

/// `held = np.repeat(y[::step], step)[:len(y)]`
pub fn hold_samples(y: &[f32], step: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(y.len());
    'outer: for v in y.iter().step_by(step) {
        for _ in 0..step {
            if out.len() == y.len() {
                break 'outer;
            }
            out.push(*v);
        }
    }
    out
}

/// `int(round(x))` as Python does it (ties to even).
#[inline]
pub fn py_round(x: f64) -> i64 {
    np::round_i64(x)
}
