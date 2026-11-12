//! Reading source audio, behind a trait so the selection code stays pure:
//! `ytp-media` implements it with ffmpeg, tests use [`NoAudio`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub const SR: f64 = 16000.0;

pub trait AudioProbe: Send + Sync {
    /// Mono 16 kHz samples in -1..1 of `source` from `start` for `duration`
    /// seconds, decoded as 16-bit PCM would be; `None` when unreadable.
    fn decode(&self, source: &str, start: f64, duration: f64) -> Option<Vec<f32>>;
}

/// No audio can be read: every measurement falls back to its stored-time default.
pub struct NoAudio;

impl AudioProbe for NoAudio {
    fn decode(&self, _: &str, _: f64, _: f64) -> Option<Vec<f32>> {
        None
    }
}

/// RMS of each whole `win`-sample window, on the int16 scale
/// (`rms_windows` in `app/ytpmv/pitch.py`).
pub fn rms_windows(x: &[f32], win: usize) -> Vec<f64> {
    let n = x.len().saturating_sub(1) / win;
    (0..n)
        .map(|k| {
            let s: f64 = x[k * win..(k + 1) * win]
                .iter()
                .map(|&v| {
                    let v = v as f64 * 32768.0;
                    v * v
                })
                .sum();
            (s / win as f64).sqrt()
        })
        .collect()
}

/// Closest zero crossing of matching slope within `window` samples of `idx`
/// (`dsp.snap_to_zero_crossing`, direction "nearest"); returns an index into
/// `audio`.
pub fn snap_to_zero_crossing(audio: &[f32], idx: i64, window: i64) -> i64 {
    let n = audio.len() as i64;
    if n < 2 {
        return idx;
    }
    let target = idx.clamp(0, n - 1);
    let a = |i: i64| audio[i as usize] as f64;
    let slope = if target < n - 1 && target > 0 {
        a(target + 1) - a(target - 1)
    } else if target < n - 1 {
        a(target + 1) - a(target)
    } else {
        a(target) - a(target - 1)
    };
    let sign = |v: f64| if v > 0.0 { 1 } else if v < 0.0 { -1 } else { 0 };
    let target_sign = sign(slope);
    let lo = (target - window).max(0);
    let hi = (target + window).min(n - 1);
    if hi <= lo {
        return target;
    }
    let (mut best_match, mut best_match_d) = (None, i64::MAX);
    let (mut best_any, mut best_any_d) = (None, i64::MAX);
    for i in lo..hi {
        let (s0, s1) = (a(i), a(i + 1));
        if s0 == 0.0 && s1 == 0.0 {
            continue;
        }
        if (s0 <= 0.0 && s1 >= 0.0) || (s0 >= 0.0 && s1 <= 0.0) {
            let cand = if s0.abs() <= s1.abs() { i } else { i + 1 };
            let dist = (cand - target).abs();
            if dist < best_any_d {
                best_any_d = dist;
                best_any = Some(cand);
            }
            if (target_sign == 0 || sign(s1 - s0) == target_sign) && dist < best_match_d {
                best_match_d = dist;
                best_match = Some(cand);
            }
        }
    }
    best_match.or(best_any).unwrap_or(target)
}

/// Memoised audio measurements for one generation.
pub struct AudioCache<'a> {
    pub probe: &'a dyn AudioProbe,
    region: Mutex<HashMap<(String, i64, i64), f64>>,
    #[allow(clippy::type_complexity)]
    env: Mutex<HashMap<(String, i64, i64), Arc<Vec<f64>>>>,
    ends: Mutex<HashMap<(String, u64, u64), f64>>,
    audible: Mutex<HashMap<(String, u64, u64), f64>>,
}

impl<'a> AudioCache<'a> {
    pub fn new(probe: &'a dyn AudioProbe) -> Self {
        Self {
            probe,
            region: Mutex::default(),
            env: Mutex::default(),
            ends: Mutex::default(),
            audible: Mutex::default(),
        }
    }

    /// Loudness of a slice (int16 scale), 0.0 if it cannot be read.
    pub fn region_rms(&self, source: &str, start: f64, duration: f64) -> f64 {
        let key = (
            source.to_string(),
            (start * 100.0).round() as i64,
            (duration * 100.0).round() as i64,
        );
        if let Some(v) = self.region.lock().unwrap().get(&key) {
            return *v;
        }
        let rms = match self.probe.decode(source, start, duration) {
            Some(x) if !x.is_empty() => {
                let s: f64 = x.iter().map(|&v| (v as f64 * 32768.0).powi(2)).sum();
                (s / x.len() as f64).sqrt()
            }
            _ => 0.0,
        };
        self.region.lock().unwrap().insert(key, rms);
        rms
    }

    /// Loudness of `a..b` in 10 ms windows; empty if unreadable.
    pub fn envelope(&self, source: &str, a: f64, b: f64) -> Arc<Vec<f64>> {
        let key = (
            source.to_string(),
            (a * 1000.0).round() as i64,
            (b * 1000.0).round() as i64,
        );
        if let Some(v) = self.env.lock().unwrap().get(&key) {
            return v.clone();
        }
        let env = Arc::new(
            self.probe
                .decode(source, a, b - a)
                .map(|x| rms_windows(&x, 160))
                .unwrap_or_default(),
        );
        self.env.lock().unwrap().insert(key, env.clone());
        env
    }

    pub fn memo_ends(&self, kind: u8, source: &str, a: f64, b: f64, f: impl FnOnce() -> f64) -> f64 {
        let key = (source.to_string(), a.to_bits(), b.to_bits());
        let table = if kind == 0 { &self.ends } else { &self.audible };
        if let Some(v) = table.lock().unwrap().get(&key) {
            return *v;
        }
        let v = f();
        table.lock().unwrap().insert(key, v);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rms_and_snap() {
        let x: Vec<f32> = (0..1600).map(|i| ((i as f32) * 0.1).sin() * 0.5).collect();
        let r = rms_windows(&x, 160);
        assert_eq!(r.len(), 9); // (1600 - 1) // 160
        assert!((r[0] - 0.5 * 32768.0 / 2f64.sqrt()).abs() < 800.0);
        let s = snap_to_zero_crossing(&x, 100, 64);
        assert!((x[s as usize]).abs() < 0.06);
    }
}
