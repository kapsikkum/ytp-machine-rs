//! The Mega Drive's YM2612 (port of `app/ytpmv/ym2612.py`): four-operator FM
//! modelled a whole note at a time, plus the 8-bit DAC channel and the
//! sampled noise drums.

use std::collections::HashMap;
use std::f64::consts::PI;
use std::sync::{Mutex, OnceLock};

use crate::gb::cdiv;
use crate::np::{self, C64};
use crate::util::{self, f32c, FftLen};

pub const MASTER_CLOCK: f64 = 53693175.0 / 7.0;
pub const FM_RATE: f64 = MASTER_CLOCK / 144.0;
pub const EG_RATE: f64 = FM_RATE / 3.0;
const AVG: [f64; 4] = [0.5, 0.625, 0.75, 0.875];
const DETUNE: [f64; 4] = [0.0, 3.4, 6.8, 10.2];
pub const LADDER_GAP: f64 = 0.003;
pub const DAC_RATE: f64 = 13300.0;

const CARRIERS: [&[usize]; 8] = [&[3], &[3], &[3], &[3], &[1, 3], &[1, 2, 3], &[1, 2, 3], &[0, 1, 2, 3]];

fn sin_table() -> &'static [i32; 256] {
    static T: OnceLock<[i32; 256]> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = [0i32; 256];
        for (i, v) in t.iter_mut().enumerate() {
            let x = -(((i as f64 + 0.5) * PI / 512.0).sin().log2()) * 256.0;
            *v = np::round_even(x).min(4095.0) as i32;
        }
        t
    })
}

fn rate(r: i64, keycode: i64, ks: i64) -> i64 {
    if r <= 0 {
        0
    } else {
        (2 * r + (keycode >> (3 - ks))).min(63)
    }
}

fn per_tick(r: i64) -> f64 {
    if r <= 0 {
        0.0
    } else {
        (AVG[(r & 3) as usize] * 2.0f64.powi((r >> 2) as i32 - 11)).min(14.0)
    }
}

/// One operator. Rates are 0-31 (rr 0-15), levels are the chip's own.
#[derive(Clone, Copy, Debug)]
pub struct Op {
    pub mul: f64,
    pub dt: i64,
    pub tl: i64,
    pub ar: i64,
    pub d1r: i64,
    pub sl: i64,
    pub d2r: i64,
    pub rr: i64,
    pub ks: i64,
}

impl Default for Op {
    fn default() -> Self {
        Op { mul: 1.0, dt: 0, tl: 0, ar: 31, d1r: 0, sl: 0, d2r: 0, rr: 7, ks: 0 }
    }
}

impl Op {
    /// Attenuation over *ticks*, in the chip's units (0 loud, 1023 silent).
    pub fn envelope(&self, n: usize, keycode: i64, off: f64) -> Vec<f64> {
        let a = per_tick(rate(self.ar, keycode, self.ks)) / 16.0;
        let d1 = per_tick(rate(self.d1r, keycode, self.ks));
        let d2 = per_tick(rate(self.d2r, keycode, self.ks));
        let rr = per_tick(rate(self.rr * 2 + 1, keycode, self.ks));
        let sl = if self.sl >= 15 { 1023.0 } else { self.sl as f64 * 32.0 };

        let t_a = if a > 0.0 && a < 1.0 { (1.0f64 / 1024.0).ln() / (1.0 - a).ln() } else { 0.0 };
        let held = |t: f64| -> f64 {
            if a <= 0.0 {
                return 1023.0;
            }
            if t < t_a {
                return (1024.0 * (1.0 - a).powf(t) - 1.0).max(0.0);
            }
            let since = (t - t_a).max(0.0);
            let fall = sl.min(d1 * since);
            let later = if d1 > 0.0 { (since - sl / d1).max(0.0) } else { 0.0 };
            (fall + d2 * later).min(1023.0)
        };

        let last = if n > 0 { (n - 1) as f64 / 3.0 } else { 0.0 };
        let release = n > 0 && off < last;
        let at_off = if release { held(off) } else { 0.0 };
        let add = self.tl as f64 * 8.0;
        (0..n)
            .map(|i| {
                let t = i as f64 / 3.0;
                let e = if release && t >= off { (at_off + rr * (t - off)).min(1023.0) } else { held(t) };
                (e + add).min(1023.0)
            })
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct Patch {
    pub name: &'static str,
    pub about: &'static str,
    pub alg: usize,
    pub fb: i64,
    pub ops: [Op; 4],
    pub bend: f64,
    pub bend_time: f64,
    pub gain: f64,
    pub base_hz: f64,
    pub transpose: i32,
}

fn op(mul: f64, dt: i64, tl: i64, ar: i64, d1r: i64, sl: i64, d2r: i64, rr: i64, ks: i64) -> Op {
    Op { mul, dt, tl, ar, d1r, sl, d2r, rr, ks }
}

fn patch(name: &'static str, about: &'static str, alg: usize, fb: i64, gain: f64, ops: [Op; 4]) -> Patch {
    Patch { name, about, alg, fb, ops, bend: 0.0, bend_time: 0.008, gain, base_hz: 0.0, transpose: 0 }
}

pub fn patches() -> &'static [Patch] {
    static P: OnceLock<Vec<Patch>> = OnceLock::new();
    P.get_or_init(build_patches)
}

pub fn patch_by_name(name: &str) -> Option<&'static Patch> {
    patches().iter().find(|p| p.name == name)
}

fn build_patches() -> Vec<Patch> {
    let mut v = Vec::new();
    v.push(Patch {
        transpose: 12,
        ..patch(
            "bass",
            "slap bass, Sonic 2's own",
            0,
            1,
            0.9,
            [
                op(9.0, 0, 37, 31, 18, 2, 0, 15, 0),
                op(0.0, 7, 48, 31, 14, 2, 4, 15, 0),
                op(0.0, 3, 19, 31, 10, 2, 4, 15, 1),
                op(0.0, 0, 4, 31, 10, 2, 3, 15, 1),
            ],
        )
    });
    v.push(patch(
        "lead",
        "a hard synth lead",
        2,
        6,
        1.24,
        [
            op(1.0, 0, 22, 31, 10, 2, 3, 10, 1),
            op(2.0, 0, 34, 31, 14, 3, 4, 10, 2),
            op(1.0, 2, 28, 31, 12, 2, 3, 10, 1),
            op(1.0, 0, 8, 31, 8, 1, 2, 8, 1),
        ],
    ));
    v.push(patch(
        "organ",
        "drawbar organ, no FM at all",
        7,
        0,
        4.43,
        [
            op(1.0, 0, 10, 31, 0, 0, 0, 8, 0),
            op(2.0, 0, 16, 31, 0, 0, 0, 8, 0),
            op(4.0, 0, 24, 31, 0, 0, 0, 8, 1),
            op(8.0, 0, 34, 31, 0, 0, 0, 8, 2),
        ],
    ));
    v.push(patch(
        "brass",
        "synth brass, with the swell",
        2,
        5,
        1.48,
        [
            op(1.0, 0, 30, 24, 8, 2, 2, 8, 1),
            op(1.0, 0, 36, 22, 10, 3, 2, 8, 1),
            op(1.0, 1, 32, 26, 9, 2, 2, 8, 1),
            op(1.0, 0, 10, 28, 6, 1, 1, 8, 1),
        ],
    ));
    v.push(patch(
        "bell",
        "a struck bell, ringing on",
        5,
        2,
        3.95,
        [
            op(3.0, 0, 28, 31, 16, 6, 8, 10, 2),
            op(1.0, 0, 12, 31, 9, 8, 5, 6, 1),
            op(5.0, 0, 24, 31, 13, 9, 7, 6, 2),
            op(7.0, 0, 32, 31, 15, 10, 9, 6, 3),
        ],
    ));
    v.push(patch(
        "piano",
        "the electric piano everyone had",
        5,
        4,
        3.12,
        [
            op(14.0, 0, 36, 31, 26, 12, 18, 12, 3),
            op(1.0, 0, 12, 31, 12, 6, 6, 9, 2),
            op(1.0, 1, 18, 31, 13, 6, 6, 9, 2),
            op(2.0, 0, 30, 31, 18, 8, 9, 9, 2),
        ],
    ));
    v.push(patch(
        "strings",
        "a soft pad to sit behind things",
        4,
        2,
        2.28,
        [
            op(1.0, 0, 38, 18, 6, 2, 1, 5, 1),
            op(1.0, 0, 14, 20, 5, 1, 1, 4, 1),
            op(2.0, 0, 42, 17, 6, 2, 1, 5, 1),
            op(1.0, 1, 16, 19, 5, 1, 1, 4, 1),
        ],
    ));
    v.push(patch(
        "dist-guitar",
        "the distorted guitar: feedback all the way up, both pairs driven",
        4,
        7,
        1.0,
        [
            op(1.0, 0, 14, 31, 6, 3, 2, 9, 1),
            op(1.0, 1, 6, 31, 4, 2, 2, 9, 1),
            op(2.0, 5, 22, 31, 8, 4, 3, 9, 1),
            op(1.0, 2, 8, 31, 4, 2, 2, 9, 1),
        ],
    ));
    v.push(patch(
        "clean-guitar",
        "a plucked guitar: a bright attack that dies back to the note",
        4,
        3,
        2.0,
        [
            op(3.0, 0, 26, 31, 20, 10, 10, 10, 2),
            op(1.0, 0, 6, 31, 9, 5, 6, 9, 1),
            op(1.0, 1, 30, 31, 16, 8, 8, 10, 2),
            op(2.0, 0, 16, 31, 11, 6, 7, 9, 1),
        ],
    ));
    v.push(Patch {
        transpose: 12,
        ..patch(
            "synth-bass",
            "a synth bass: fat, buzzy and held",
            4,
            5,
            1.3,
            [
                op(1.0, 0, 18, 31, 10, 5, 2, 12, 0),
                op(1.0, 0, 4, 31, 6, 2, 1, 12, 0),
                op(1.0, 1, 28, 31, 14, 8, 3, 12, 0),
                op(0.5, 0, 10, 31, 6, 2, 1, 12, 0),
            ],
        )
    });
    v.push(patch(
        "square-lead",
        "a square lead: odd harmonics only, like the handhelds",
        4,
        0,
        1.0,
        [
            op(2.0, 0, 34, 31, 0, 0, 0, 9, 0),
            op(1.0, 0, 6, 31, 2, 1, 1, 9, 0),
            op(2.0, 1, 36, 31, 0, 0, 0, 9, 0),
            op(1.0, 1, 10, 31, 2, 1, 1, 9, 0),
        ],
    ));
    v.push(patch(
        "reed",
        "a reed: sax, clarinet, harmonica, with the buzz up front",
        2,
        4,
        1.0,
        [
            op(3.0, 0, 34, 26, 8, 4, 2, 9, 1),
            op(1.0, 0, 28, 25, 6, 3, 1, 9, 1),
            op(2.0, 0, 32, 26, 7, 3, 1, 9, 1),
            op(1.0, 0, 8, 24, 4, 1, 1, 9, 1),
        ],
    ));
    v.push(patch(
        "flute",
        "a flute or whistle: nearly a sine, breathed into",
        4,
        6,
        1.6,
        [
            op(4.0, 0, 56, 26, 10, 6, 4, 8, 1),
            op(1.0, 0, 8, 22, 2, 1, 1, 8, 1),
            op(1.0, 0, 44, 24, 4, 2, 1, 8, 1),
            op(2.0, 0, 34, 22, 2, 1, 1, 8, 1),
        ],
    ));
    v.push(patch(
        "marimba",
        "mallets: a woody knock gone in a moment",
        4,
        0,
        2.4,
        [
            op(4.0, 0, 30, 31, 24, 15, 0, 12, 2),
            op(1.0, 0, 4, 31, 15, 10, 12, 10, 2),
            op(10.0, 0, 40, 31, 28, 15, 0, 12, 2),
            op(3.0, 0, 26, 31, 20, 12, 14, 10, 2),
        ],
    ));
    v.push(patch(
        "choir",
        "voices: a slow, hollow aah",
        4,
        1,
        1.3,
        [
            op(2.0, 0, 40, 16, 3, 2, 1, 6, 1),
            op(1.0, 0, 10, 15, 2, 1, 1, 5, 1),
            op(3.0, 2, 46, 16, 3, 2, 1, 6, 1),
            op(1.0, 6, 12, 15, 2, 1, 1, 5, 1),
        ],
    ));
    v.push(patch(
        "stab",
        "an orchestra hit: everything at once, then gone",
        5,
        6,
        2.2,
        [
            op(1.0, 0, 16, 31, 14, 8, 10, 12, 1),
            op(1.0, 0, 6, 31, 12, 8, 12, 12, 1),
            op(2.0, 1, 10, 31, 12, 8, 12, 12, 1),
            op(3.0, 5, 14, 31, 13, 9, 12, 12, 1),
        ],
    ));
    v.push(Patch {
        base_hz: 54.0,
        bend: 30.0,
        bend_time: 0.012,
        ..patch(
            "md-kick",
            "a sine dropped hard, the FM kick",
            0,
            0,
            0.77,
            [
                op(1.0, 0, 40, 31, 26, 4, 20, 12, 0),
                op(1.0, 0, 64, 31, 31, 15, 31, 15, 0),
                op(1.0, 0, 64, 31, 31, 15, 31, 15, 0),
                op(1.0, 0, 2, 31, 18, 15, 0, 10, 0),
            ],
        )
    });
    v.push(Patch {
        base_hz: 196.0,
        ..patch(
            "md-snare",
            "a snare sample off the DAC, as Sonic played its drums",
            4,
            7,
            1.9,
            [
                op(15.0, 0, 22, 31, 24, 15, 0, 12, 0),
                op(14.0, 0, 10, 31, 17, 15, 0, 12, 0),
                op(11.0, 0, 26, 31, 26, 15, 0, 12, 0),
                op(13.0, 0, 14, 31, 17, 15, 0, 12, 0),
            ],
        )
    });
    v.push(Patch {
        base_hz: 880.0,
        ..patch(
            "md-hat",
            "a hi-hat sample off the DAC, over almost before it starts",
            4,
            7,
            3.03,
            [
                op(15.0, 0, 20, 31, 24, 15, 0, 15, 0),
                op(13.0, 0, 16, 31, 20, 15, 0, 15, 0),
                op(14.0, 0, 24, 31, 24, 15, 0, 15, 0),
                op(15.0, 0, 18, 31, 20, 15, 0, 15, 0),
            ],
        )
    });
    v.push(Patch {
        base_hz: 130.0,
        bend: 14.0,
        bend_time: 0.03,
        ..patch(
            "md-tom",
            "a kick with less of a drop, and tuned",
            0,
            1,
            1.06,
            [
                op(2.0, 0, 38, 31, 22, 5, 14, 12, 0),
                op(1.0, 0, 60, 31, 28, 15, 31, 15, 0),
                op(1.0, 0, 44, 31, 20, 6, 12, 12, 0),
                op(1.0, 0, 6, 31, 16, 15, 0, 10, 0),
            ],
        )
    });
    v.push(Patch {
        base_hz: 1100.0,
        ..patch(
            "md-cymbal",
            "a cymbal sample off the DAC, left to ring",
            4,
            7,
            2.43,
            [
                op(15.0, 0, 24, 31, 16, 15, 0, 6, 0),
                op(14.0, 0, 14, 31, 13, 15, 0, 5, 0),
                op(13.0, 0, 28, 31, 16, 15, 0, 6, 0),
                op(11.0, 0, 18, 31, 13, 15, 0, 5, 0),
            ],
        )
    });
    v.push(Patch {
        base_hz: 420.0,
        ..patch(
            "md-perc",
            "a tick and nothing after it",
            4,
            6,
            2.79,
            [
                op(15.0, 0, 26, 31, 24, 15, 0, 15, 0),
                op(9.0, 0, 16, 31, 20, 15, 0, 15, 0),
                op(12.0, 0, 30, 31, 24, 15, 0, 15, 0),
                op(7.0, 0, 20, 31, 20, 15, 0, 15, 0),
            ],
        )
    });
    v
}

pub const DRUMS: &[(&str, &str)] = &[
    ("kick", "md-kick"),
    ("snare", "md-snare"),
    ("hats", "md-hat"),
    ("toms", "md-tom"),
    ("cymbals", "md-cymbal"),
    ("perc", "md-perc"),
];

pub const SAMPLED_DRUMS: [&str; 3] = ["md-snare", "md-hat", "md-cymbal"];

/// Four whole cycles of phase is the chip's deepest modulation. 14-bit signed output.
fn fm_op(phase: &[f64], env: &[f64], modulator: Option<&[f64]>) -> Vec<f64> {
    let sin = sin_table();
    (0..phase.len())
        .map(|i| {
            let mut idx = (phase[i] * 1024.0).floor() as i64;
            if let Some(m) = modulator {
                idx += (m[i] * 0.5) as i64;
            }
            idx &= 0x3FF;
            let quarter = (idx & 0xFF) as usize;
            let q = if idx & 0x100 != 0 { 0xFF - quarter } else { quarter };
            let att = sin[q] as f64 + env[i] * 4.0;
            let amp = (-att.min(8191.0) / 256.0).exp2() * 8192.0;
            (if idx & 0x200 != 0 { -amp } else { amp }).trunc()
        })
        .collect()
}

/// Operator 1, which on this chip can modulate itself (solved at its fixed point).
fn feedback(phase: &[f64], env: &[f64], fb: i64) -> Vec<f64> {
    if fb <= 0 {
        return fm_op(phase, env, None);
    }
    let beta = PI * 2.0f64.powi(fb as i32 - 7);
    let two_pi = 2.0 * PI;
    let mut shifted = Vec::with_capacity(phase.len());
    for i in 0..phase.len() {
        let depth = beta * (-env[i] * 4.0 / 256.0).exp2();
        let theta = two_pi * phase[i];
        let mut y = theta.sin();
        for _ in 0..8 {
            y = (theta + depth * y).sin();
        }
        shifted.push(phase[i] + depth * y / two_pi);
    }
    fm_op(&shifted, env, None)
}

fn add(a: &[f64], b: &[f64]) -> Vec<f64> {
    a.iter().zip(b.iter()).map(|(x, y)| x + y).collect()
}

fn voice(phase: &[Vec<f64>; 4], env: &[Vec<f64>; 4], alg: usize, fb: i64) -> Vec<f64> {
    let o1 = feedback(&phase[0], &env[0], fb);
    let (p, e) = (phase, env);
    match alg {
        0 => {
            let a = fm_op(&p[1], &e[1], Some(&o1));
            let b = fm_op(&p[2], &e[2], Some(&a));
            fm_op(&p[3], &e[3], Some(&b))
        }
        1 => {
            let a = fm_op(&p[1], &e[1], None);
            let b = fm_op(&p[2], &e[2], Some(&add(&o1, &a)));
            fm_op(&p[3], &e[3], Some(&b))
        }
        2 => {
            let a = fm_op(&p[1], &e[1], None);
            let b = fm_op(&p[2], &e[2], Some(&a));
            fm_op(&p[3], &e[3], Some(&add(&o1, &b)))
        }
        3 => {
            let a = fm_op(&p[1], &e[1], Some(&o1));
            let b = fm_op(&p[2], &e[2], None);
            fm_op(&p[3], &e[3], Some(&add(&a, &b)))
        }
        4 => {
            let a = fm_op(&p[1], &e[1], Some(&o1));
            let b = fm_op(&p[2], &e[2], None);
            let c = fm_op(&p[3], &e[3], Some(&b));
            add(&a, &c)
        }
        5 => {
            let a = fm_op(&p[1], &e[1], Some(&o1));
            let b = fm_op(&p[2], &e[2], Some(&o1));
            let c = fm_op(&p[3], &e[3], Some(&o1));
            add(&add(&a, &b), &c)
        }
        6 => {
            let a = fm_op(&p[1], &e[1], Some(&o1));
            let b = fm_op(&p[2], &e[2], None);
            let c = fm_op(&p[3], &e[3], None);
            add(&add(&a, &b), &c)
        }
        _ => {
            let a = fm_op(&p[1], &e[1], None);
            let b = fm_op(&p[2], &e[2], None);
            let c = fm_op(&p[3], &e[3], None);
            add(&add(&add(&o1, &a), &b), &c)
        }
    }
}

fn lowpass(y: &[f64], hz: f64, sr: f64) -> Vec<f64> {
    util::lowpass8_f64(y, hz, sr, FftLen::Double)
}

fn highpass_f32(y: &[f32], hz: f64, sr: f64) -> Vec<f64> {
    if y.len() < 32 || hz <= 0.0 {
        return y.iter().map(|v| *v as f64).collect();
    }
    let n = util::pow2_ceil(y.len() * 2);
    let f = np::rfftfreq(n, 1.0 / sr);
    let mut spec = np::rfft_f32(y, n);
    for (s, fr) in spec.iter_mut().zip(f.iter()) {
        *s *= 1.0 - 1.0 / (1.0 + (fr / hz).powf(4.0));
    }
    let mut out = np::irfft(&spec, n);
    out.truncate(y.len());
    out
}

fn to_sr(x: &[f64], src: f64, dst: f64) -> Vec<f32> {
    if (src - dst).abs() < 1e-6 || x.len() < 2 {
        return x.iter().map(|v| *v as f32).collect();
    }
    let filtered;
    let x = if dst < src {
        filtered = lowpass(x, 0.45 * dst, src);
        &filtered[..]
    } else {
        x
    };
    let n = (util::py_round(x.len() as f64 * dst / src).max(1)) as usize;
    let pos: Vec<f64> = (0..n).map(|i| i as f64 * src / dst).collect();
    np::interp_arange(&pos, x).iter().map(|v| *v as f32).collect()
}

/// The dead zone the chip's output never quite crosses.
pub fn ladder(y: &[f32], gap: f64) -> Vec<f32> {
    if gap <= 0.0 || y.is_empty() {
        return y.to_vec();
    }
    let g = f32c(gap);
    let eps = f32c(1e-6);
    y.iter()
        .map(|v| {
            if v.abs() < eps {
                *v
            } else {
                let sign = if *v > 0.0 { 1.0f32 } else if *v < 0.0 { -1.0 } else { 0.0 };
                *v + sign * g
            }
        })
        .collect()
}

/// What comes out of the console, rather than out of the chip.
pub fn output(y: &[f32], sr: u32, gap: f64) -> Vec<f32> {
    highpass_f32(&ladder(y, gap), 25.0, sr as f64).iter().map(|v| *v as f32).collect()
}

/// Output at *rate* samples a second, held flat between them.
pub fn hold(y: &[f32], rate: f64, sr: u32) -> Vec<f32> {
    let step = (util::py_round(sr as f64 / rate).max(1)) as usize;
    if step == 1 {
        return y.to_vec();
    }
    util::hold_samples(y, step)
}

/// The chip's PCM channel: a sample played the way the Mega Drive played one.
pub fn dac(y: &[f32], rate: f64, sr: u32, gap: f64) -> Vec<f32> {
    if y.is_empty() {
        return Vec::new();
    }
    let c = f32c(128.0);
    let held: Vec<f32> = hold(y, rate, sr).iter().map(|v| (*v * c).round_ties_even() / c).collect();
    output(&held, sr, gap)
}

// ── Drums off the sample channel ─────────────────────────────────────────────

fn band_noise(n: usize, sr: f64, lo: f64, hi: f64, seed: u64) -> Vec<f64> {
    let mut rng = np::Rng::new(seed);
    let noise = rng.standard_normal_vec(n);
    let mut spec = np::rfft(&noise, n);
    let fr = np::rfftfreq(n, 1.0 / sr);
    for (s, f) in spec.iter_mut().zip(fr.iter()) {
        let shape = 1.0 / (1.0 + (lo / f.max(1.0)).powf(4.0)) / (1.0 + (f / hi).powf(4.0));
        *s *= shape;
    }
    let y = np::irfft(&spec, n);
    let peak = np::abs_max_f64(&y) + 1e-12;
    y.iter().map(|v| v / peak).collect()
}

fn drum_sample(name: &str, sr: u32) -> std::sync::Arc<Vec<f32>> {
    static CACHE: OnceLock<Mutex<HashMap<(String, u32), std::sync::Arc<Vec<f32>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().unwrap().get(&(name.to_string(), sr)) {
        return hit.clone();
    }
    let srf = sr as f64;
    let (n, mut y): (usize, Vec<f64>) = match name {
        "md-snare" => {
            let n = (0.30 * srf) as usize;
            let t: Vec<f64> = (0..n).map(|i| i as f64 / srf).collect();
            let mut cum = 0.0f64;
            let body: Vec<f64> = t
                .iter()
                .map(|&tv| {
                    cum += 180.0 + 110.0 * (-tv / 0.010).exp();
                    (2.0 * PI * cum / srf).sin() * (-tv / 0.045).exp()
                })
                .collect();
            let wires = band_noise(n, srf, 1400.0, 9000.0, 7);
            let y = (0..n)
                .map(|i| {
                    let w = wires[i] * (0.55 * (-t[i] / 0.012).exp() + 0.75 * (-t[i] / 0.085).exp());
                    0.62 * body[i] + w
                })
                .collect();
            (n, y)
        }
        "md-hat" => {
            let n = (0.12 * srf) as usize;
            let noise = band_noise(n, srf, 6500.0, 16000.0, 11);
            let y = (0..n).map(|i| noise[i] * (-(i as f64 / srf) / 0.022).exp()).collect();
            (n, y)
        }
        _ => {
            let n = (1.1 * srf) as usize;
            let noise = band_noise(n, srf, 3200.0, 15000.0, 13);
            let y = (0..n)
                .map(|i| {
                    let t = i as f64 / srf;
                    let mut ring = 0.0f64;
                    for hz in [3150.0f64, 4730.0, 6410.0] {
                        ring += (2.0 * PI * hz * t).sin();
                    }
                    let ring = ring / 3.0;
                    (noise[i] + 0.18 * ring) * (0.35 * (-t / 0.03).exp() + 0.65 * (-t / 0.38).exp())
                })
                .collect();
            (n, y)
        }
    };
    let attack = n.min((0.0015 * srf) as usize);
    let ramp = np::linspace(0.0, 1.0, attack);
    for i in 0..attack {
        y[i] *= ramp[i];
    }
    let peak = np::abs_max_f64(&y) + 1e-12;
    let out: Vec<f32> = y.iter().map(|v| (0.9 * v / peak) as f32).collect();
    let arc = std::sync::Arc::new(out);
    cache.lock().unwrap().insert((name.to_string(), sr), arc.clone());
    arc
}

fn play_sample(name: &str, dur: f64, sr: u32, level: f64, tail: f64) -> Vec<f32> {
    let y = drum_sample(name, sr);
    let n = y.len().min((util::py_round((dur + tail.max(0.0)) * sr as f64).max(2)) as usize);
    let lv = f32c(level.clamp(0.0, 1.0));
    let scaled: Vec<f32> = y[..n].iter().map(|v| *v * lv).collect();
    let mut out = dac(&scaled, DAC_RATE, sr, LADDER_GAP);
    let edge = out.len().min((0.004 * sr as f64) as usize);
    if n < y.len() && edge > 1 {
        util::fade_tail_f32(&mut out, edge);
    }
    out
}

/// *patch* playing *hz*, held down for *dur* seconds, as the chip would.
pub fn render_note(patch: &Patch, hz: f64, dur: f64, sr: u32, level: f64, tail: f64) -> Vec<f32> {
    if SAMPLED_DRUMS.contains(&patch.name) {
        return play_sample(patch.name, dur, sr, level, tail);
    }
    let n = (util::py_round((dur + tail.max(0.0)) * FM_RATE).max(2)) as usize;
    let base = if patch.base_hz != 0.0 { patch.base_hz } else { hz };
    let hz = base * 2.0f64.powf(patch.transpose as f64 / 12.0);
    let midi = 12.0 * (hz.max(1e-6) / 440.0).log2() + 69.0;
    let keycode = (util::py_round(midi).div_euclid(3)).clamp(0, 31);

    let f: Vec<f64> = (0..n)
        .map(|i| {
            if patch.bend != 0.0 {
                let t = i as f64 / FM_RATE;
                hz * 2.0f64.powf(patch.bend * (-t / patch.bend_time).exp() / 12.0)
            } else {
                hz
            }
        })
        .collect();

    let quieter = if level >= 1.0 { 0.0 } else { (-20.0 * level.max(1e-3).log10() / 0.75).min(96.0) };
    let carriers = CARRIERS[patch.alg];

    let mut phase: [Vec<f64>; 4] = Default::default();
    let mut env: [Vec<f64>; 4] = Default::default();
    for (i, o) in patch.ops.iter().enumerate() {
        let cents = DETUNE[(o.dt & 3) as usize] * (if o.dt < 4 { 1.0 } else { -1.0 }) * (1.0 + keycode as f64 / 32.0);
        let mul = if o.mul == 0.0 { 0.5 } else { o.mul };
        let c = 2.0f64.powf(cents / 1200.0);
        let mut cum = 0.0f64;
        phase[i] = f
            .iter()
            .map(|fv| {
                cum += fv * mul * c;
                cum / FM_RATE
            })
            .collect();
        let e = o.envelope(n, keycode, dur * EG_RATE);
        env[i] = if carriers.contains(&i) {
            e.iter().map(|v| (v + quieter * 8.0).min(1023.0)).collect()
        } else {
            e
        };
    }

    let nc = carriers.len() as f64;
    let mixed: Vec<f64> =
        voice(&phase, &env, patch.alg, patch.fb).iter().map(|v| v / 8192.0 / nc * patch.gain).collect();
    let y = to_sr(&mixed, FM_RATE, sr as f64);
    let mut y = output(&y, sr, LADDER_GAP);
    let edge = y.len().min((0.003 * sr as f64) as usize);
    util::fade_tail_f32(&mut y, edge);
    y
}

pub fn patch_for(role: &str, program: Option<u8>) -> &'static Patch {
    const BY_PROGRAM: &[(u8, u8, &str)] = &[
        (0, 7, "piano"), (8, 10, "bell"), (11, 13, "marimba"), (14, 15, "bell"),
        (16, 20, "organ"), (21, 23, "reed"), (24, 28, "clean-guitar"), (29, 31, "dist-guitar"),
        (32, 37, "bass"), (38, 39, "synth-bass"), (40, 44, "strings"), (45, 46, "clean-guitar"),
        (47, 47, "marimba"), (48, 51, "strings"), (52, 54, "choir"), (55, 55, "stab"),
        (56, 63, "brass"), (64, 71, "reed"), (72, 79, "flute"), (80, 80, "square-lead"),
        (81, 81, "lead"), (82, 83, "flute"), (84, 84, "clean-guitar"), (85, 85, "choir"),
        (86, 87, "lead"), (88, 95, "strings"), (96, 103, "bell"), (104, 107, "clean-guitar"),
        (108, 108, "marimba"), (109, 111, "reed"), (112, 119, "marimba"), (120, 127, "lead"),
    ];
    let head = role.split(':').next().unwrap_or("");
    if head == "drums" {
        let group = role.split(':').next_back().unwrap_or("");
        let name = DRUMS.iter().find(|(g, _)| *g == group).map(|(_, n)| *n).unwrap_or("md-perc");
        return patch_by_name(name).unwrap();
    }
    let named = program.and_then(|p| BY_PROGRAM.iter().find(|(lo, hi, _)| *lo <= p && p <= *hi).map(|(_, _, n)| *n));
    if head == "bass" {
        return patch_by_name(if named == Some("synth-bass") { "synth-bass" } else { "bass" }).unwrap();
    }
    if let Some(n) = named {
        if n != "piano" {
            return patch_by_name(n).unwrap();
        }
    }
    let by_role = match head {
        "bass" => Some("bass"),
        "lead" => Some("lead"),
        "chords" => Some("organ"),
        "rhythm" => Some("brass"),
        _ => None,
    };
    patch_by_name(by_role.or(named).unwrap_or("lead")).unwrap()
}

// `cdiv`/`C64` kept for future filter work parity with the other chips.
#[allow(dead_code)]
fn _unused(_: C64) -> C64 {
    cdiv(C64::new(1.0, 0.0), C64::new(1.0, 0.0))
}
