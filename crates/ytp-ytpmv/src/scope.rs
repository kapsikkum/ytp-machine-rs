//! The picture when there is no voice: every part as its own oscilloscope
//! (`app/ytpmv/scope.py`).
//!
//! With nothing recorded there is no clip to cut to, so each tile shows what
//! its part is actually putting out, the way chiptune videos draw a channel —
//! a trace in neon red on black, held still by triggering on a rising zero
//! crossing. It is drawn at the console's own resolution and then goes
//! through that console's palette like any other picture.

use crate::SR;

/// Seconds of sound across a tile, when the part has no pitch.
const WINDOW: f64 = 0.03;
/// Otherwise this many cycles of its middle note.
const PERIODS: f64 = 4.0;
/// How far on to look for a trigger: a 50 Hz period.
const SEARCH: f64 = 0.02;

/// (trace, trace on a hit, glow, axis).
type Colours = ([f32; 3], [f32; 3], [f32; 3], [f32; 3]);

const RED: Colours = ([255.0, 40.0, 40.0], [255.0, 120.0, 110.0], [110.0, 0.0, 22.0], [34.0, 0.0, 8.0]);
// The Game Boy has no red to land on, so it draws in its lightest shade.
const GB: Colours = ([255.0, 255.0, 255.0], [255.0, 255.0, 255.0], [150.0, 150.0, 150.0], [70.0, 70.0, 70.0]);

/// The value at percentile `q` (0..100), linearly interpolated like numpy.
fn percentile(vals: &mut [f32], q: f64) -> f32 {
    if vals.is_empty() {
        return 0.0;
    }
    let pos = q / 100.0 * (vals.len() - 1) as f64;
    let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
    vals.select_nth_unstable_by(hi, |a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let vhi = vals[hi];
    let vlo = if lo == hi {
        vhi
    } else {
        vals[..hi].iter().cloned().fold(f32::MIN, f32::max)
    };
    vlo + (vhi - vlo) * (pos - lo as f64) as f32
}

/// The lowest and highest point in each column of each frame, flattened:
/// `traces[(frame * width + column) * 2 + {0: low, 1: high}]`.
///
/// `y` is the part on its own, mono. Each column keeps its range rather than
/// one sample, so a note too high to draw as a line comes out as the band it
/// fills instead of an aliased zigzag, and columns are widened to meet their
/// neighbours so the trace never breaks. `hz` is the part's middle note,
/// which sets how much of the sound a tile holds.
pub fn traces(y: &[f32], n_frames: usize, spf: usize, width: usize, hz: Option<f64>) -> Vec<f32> {
    let width = width.max(1);
    let window = match hz {
        Some(h) if h > 0.0 => (PERIODS / h).clamp(0.006, 0.04),
        _ => WINDOW,
    };
    let span = width.max((window * SR as f64) as usize / width * width);
    let search = (SEARCH * SR as f64) as usize;
    let mut mags: Vec<f32> = y.iter().map(|v| v.abs()).collect();
    let peak = percentile(&mut mags, 99.9);
    drop(mags);
    let scale = if peak > 1e-6 { 0.9 / peak } else { 0.0 };
    let mut padded: Vec<f32> = Vec::with_capacity(y.len() + span + search + 1);
    padded.extend(y.iter().map(|v| v * scale));
    padded.resize(y.len() + span + search + 1, 0.0);

    let col = span / width;
    let mut out = vec![0.0f32; n_frames * width * 2];
    let mut lo = vec![0.0f32; width];
    let mut hi = vec![0.0f32; width];
    for f in 0..n_frames {
        let c = f * spf;
        let head_end = (c + search + 1).min(padded.len());
        let head = &padded[c.min(padded.len())..head_end];
        let rise = (0..head.len().saturating_sub(1)).find(|&i| head[i] <= 0.0 && head[i + 1] > 0.0);
        let a = c + rise.map_or(0, |r| r + 1);
        for k in 0..width {
            let s = (a + k * col).min(padded.len());
            let e = (a + (k + 1) * col).min(padded.len());
            let slice = &padded[s..e];
            lo[k] = slice.iter().cloned().fold(f32::INFINITY, f32::min);
            hi[k] = slice.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            if slice.is_empty() {
                lo[k] = 0.0;
                hi[k] = 0.0;
            }
        }
        if width > 1 {
            let (lo_next, hi_next) = (lo[1..].to_vec(), hi[1..].to_vec());
            for k in 0..width - 1 {
                lo[k] = lo[k].min(hi_next[k]);
                hi[k] = hi[k].max(lo_next[k]);
            }
        }
        let base = f * width * 2;
        for k in 0..width {
            out[base + k * 2] = lo[k];
            out[base + k * 2 + 1] = hi[k];
        }
    }
    out
}

/// One tile, `h` rows by `w` columns of RGB: the trace, its glow, and the axis
/// it sits on. `trace` is one frame of [`traces`] (`width * 2` values).
pub fn draw(trace: &[f32], w: usize, h: usize, level: f32, hot: bool, screen: &str) -> Vec<u8> {
    let (neon, hotc, glow_c, axis) = if screen == "gb" { GB } else { RED };
    let mut img = vec![0.0f32; h * w * 3];
    let mid = (h as f64 - 1.0) / 2.0;
    let amp = (h as f64 / 2.0 - 2.0).max(1.0);
    let axis_row = mid.round_ties_even() as usize;
    for x in 0..w {
        img[(axis_row.min(h - 1) * w + x) * 3..][..3].copy_from_slice(&axis);
    }
    for x in 0..w.min(trace.len() / 2) {
        let (lo, hi) = (trace[x * 2] as f64, trace[x * 2 + 1] as f64);
        let top = (mid - hi * amp).round_ties_even().clamp(0.0, h as f64 - 1.0);
        let bot = (mid - lo * amp).round_ties_even().clamp(0.0, h as f64 - 1.0);
        for row in 0..h {
            let r = row as f64;
            let core = r >= top && r <= bot;
            let glow = r >= top - 1.0 && r <= bot + 1.0;
            let colour = if core {
                if hot {
                    Some(hotc)
                } else {
                    Some(neon)
                }
            } else if glow {
                Some(glow_c)
            } else {
                None
            };
            if let Some(c) = colour {
                img[(row * w + x) * 3..][..3].copy_from_slice(&c);
            }
        }
    }
    if level != 1.0 {
        for v in img.iter_mut() {
            *v *= level;
        }
    }
    img.iter().map(|v| *v as u8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sine_draws_a_band_around_the_axis() {
        let sr = SR as f64;
        let y: Vec<f32> = (0..SR as usize).map(|i| (i as f64 / sr * 440.0 * std::f64::consts::TAU).sin() as f32).collect();
        let spf = SR as usize / 25;
        let t = traces(&y, 25, spf, 64, Some(440.0));
        assert_eq!(t.len(), 25 * 64 * 2);
        let frame = &t[10 * 128..11 * 128];
        assert!(frame.iter().any(|v| *v > 0.5) && frame.iter().any(|v| *v < -0.5));
        let tile = draw(frame, 64, 36, 1.0, false, "");
        assert_eq!(tile.len(), 64 * 36 * 3);
        assert!(tile.as_chunks::<3>().0.contains(&[255, 40, 40]));
    }

    #[test]
    fn silence_is_a_flat_line() {
        let t = traces(&vec![0.0; 44100], 5, 1764, 16, None);
        assert!(t.iter().all(|v| *v == 0.0));
        let tile = draw(&t[..32], 16, 9, 1.0, false, "");
        let lit = tile.as_chunks::<3>().0.iter().filter(|p| p[0] > 200).count();
        assert!(lit >= 16);
    }

    #[test]
    fn percentile_matches_numpy() {
        let mut v: Vec<f32> = (0..1001).map(|i| i as f32).collect();
        assert!((percentile(&mut v, 99.9) - 999.0).abs() < 1e-3);
        let mut w = vec![1.0f32, 2.0, 3.0, 4.0];
        assert!((percentile(&mut w, 50.0) - 2.5).abs() < 1e-6);
    }
}
