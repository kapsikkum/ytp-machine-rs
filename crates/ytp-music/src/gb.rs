//! The Game Boy / Game Boy Color sound chip (port of `app/ytpmv/gb.py`):
//! two pulses (one with sweep), the wave channel, the noise LFSR, the 4-bit
//! envelope, and the DC-blocking output capacitor.

use std::sync::OnceLock;

use crate::np::{self, C64};
use crate::util::{self, f32c, FftLen};

pub const CLOCK: f64 = 4194304.0;
pub const CHARGE: f64 = 0.999958;
pub const OVERSAMPLE: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Pulse,
    Wave,
    Noise,
}

#[derive(Clone, Copy, Debug)]
pub struct Voice {
    pub name: &'static str,
    pub about: &'static str,
    pub kind: Kind,
    pub duty: i32,
    pub table: &'static [u8],
    pub level: i32,
    pub volume: i32,
    pub pace: i32,
    /// (pace, shift, down)
    pub sweep: Option<(i32, i32, bool)>,
    pub divider: i32,
    pub shift: i32,
    pub short: bool,
    pub base_hz: f64,
    pub gain: f64,
}

const fn v(name: &'static str, about: &'static str, kind: Kind) -> Voice {
    Voice {
        name,
        about,
        kind,
        duty: 2,
        table: &[],
        level: 1,
        volume: 15,
        pace: 0,
        sweep: None,
        divider: 0,
        shift: 4,
        short: false,
        base_hz: 0.0,
        gain: 1.0,
    }
}

pub static TABLE_TRIANGLE: [u8; 32] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0,
];
pub static TABLE_SAW: [u8; 32] = [
    0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13, 14, 14, 15, 15,
];
pub static TABLE_BASS: [u8; 32] = [
    15, 15, 14, 13, 11, 9, 7, 5, 3, 2, 1, 0, 0, 0, 1, 2, 3, 5, 7, 9, 11, 13, 14, 15, 15, 15, 15, 14, 12, 10, 8, 6,
];

pub static VOICES: &[Voice] = &[
    Voice { duty: 1, ..v("gb-pulse", "a quarter-width square", Kind::Pulse) },
    Voice { duty: 0, volume: 12, ..v("gb-pulse-thin", "an eighth-width square", Kind::Pulse) },
    Voice { duty: 2, volume: 11, ..v("gb-pulse-full", "a half-width square", Kind::Pulse) },
    Voice { duty: 1, pace: 2, ..v("gb-pluck", "a square that steps down as it plays", Kind::Pulse) },
    Voice { table: &TABLE_BASS, ..v("gb-wave", "the wave channel with a round bass table", Kind::Wave) },
    Voice {
        duty: 2,
        pace: 1,
        sweep: Some((1, 1, true)),
        base_hz: 180.0,
        ..v("gb-kick", "a square swept down, the Game Boy kick", Kind::Pulse)
    },
    Voice {
        divider: 1,
        shift: 3,
        pace: 1,
        ..v("gb-snare", "the long register, stepped down fast", Kind::Noise)
    },
    Voice {
        divider: 0,
        shift: 1,
        volume: 9,
        pace: 1,
        ..v("gb-hat", "the long register, clocked fast and cut", Kind::Noise)
    },
    Voice {
        duty: 2,
        pace: 2,
        sweep: Some((2, 2, true)),
        base_hz: 150.0,
        ..v("gb-tom", "a square swept down less hard", Kind::Pulse)
    },
    Voice {
        divider: 0,
        shift: 2,
        volume: 10,
        pace: 5,
        ..v("gb-cymbal", "the long register, left to ring", Kind::Noise)
    },
    Voice {
        divider: 2,
        shift: 3,
        short: true,
        volume: 11,
        pace: 1,
        ..v("gb-perc", "the short register, which has a pitch", Kind::Noise)
    },
];

pub const DRUMS: &[(&str, &str)] = &[
    ("kick", "gb-kick"),
    ("snare", "gb-snare"),
    ("hats", "gb-hat"),
    ("toms", "gb-tom"),
    ("cymbals", "gb-cymbal"),
    ("perc", "gb-perc"),
];

pub fn voice(name: &str) -> Option<&'static Voice> {
    VOICES.iter().find(|v| v.name == name)
}

/// Complex division as numpy does it (Smith's algorithm).
pub(crate) fn cdiv(a: C64, b: C64) -> C64 {
    let (br, bi) = (b.re.abs(), b.im.abs());
    if br >= bi {
        if br == 0.0 && bi == 0.0 {
            return C64::new(a.re / br, a.im / br);
        }
        let rat = b.im / b.re;
        let scl = 1.0 / (b.re + b.im * rat);
        C64::new((a.re + a.im * rat) * scl, (a.im - a.re * rat) * scl)
    } else {
        let rat = b.re / b.im;
        let scl = 1.0 / (b.re * rat + b.im);
        C64::new((a.re * rat + a.im) * scl, (a.im * rat - a.re) * scl)
    }
}

/// The output capacitor: a one-pole high-pass applied through its frequency response.
pub fn capacitor(y: &[f32], sr: u32) -> Vec<f32> {
    if y.len() < 32 {
        return y.to_vec();
    }
    let sr_f = sr as f64;
    let k = CHARGE.powf(CLOCK / sr_f);
    let pad = (8.0 / (1.0 - k).max(1e-9)) as usize;
    let n = util::pow2_ceil(y.len() + pad.min(1 << 20));
    let freqs = np::rfftfreq(n, 1.0 / sr_f);
    let mut spec = np::rfft_f32(y, n);
    for (s, f) in spec.iter_mut().zip(freqs.iter()) {
        let theta = -2.0 * std::f64::consts::PI * f / sr_f;
        let z = C64::new(theta.cos(), theta.sin());
        let h = cdiv(C64::new(1.0, 0.0) - z, C64::new(1.0, 0.0) - z * k);
        *s *= h;
    }
    let mut out = np::irfft(&spec, n);
    out.truncate(y.len());
    out.iter().map(|v| *v as f32).collect()
}

/// Digital 0..15 to analog +1..-1 (the slope really is negative).
pub fn dac(digital: &[f32]) -> Vec<f32> {
    let d = f32c(7.5);
    digital.iter().map(|x| 1.0f32 - *x / d).collect()
}

pub fn lfsr_bits(short: bool) -> &'static [f32] {
    static LONG: OnceLock<Vec<f32>> = OnceLock::new();
    static SHORT: OnceLock<Vec<f32>> = OnceLock::new();
    let cell = if short { &SHORT } else { &LONG };
    cell.get_or_init(|| {
        let mut reg: u32 = 0;
        let mut seen = std::collections::HashMap::new();
        let mut out: Vec<u32> = Vec::new();
        while !seen.contains_key(&reg) {
            seen.insert(reg, out.len());
            let bit = 1 - ((reg ^ (reg >> 1)) & 1);
            reg = (reg & !(1 << 15)) | (bit << 15);
            if short {
                reg = (reg & !(1 << 7)) | (bit << 7);
            }
            reg >>= 1;
            out.push(reg & 1);
        }
        let start = seen[&reg];
        out[start..].iter().map(|b| *b as f32).collect()
    })
}

pub fn floor_hz(kind: Kind) -> f64 {
    match kind {
        Kind::Pulse => CLOCK / 32.0 / 2048.0,
        Kind::Wave => CLOCK / 64.0 / 2048.0,
        Kind::Noise => 0.0,
    }
}

fn period(hz: f64, top: f64) -> f64 {
    np::clip(np::round_even(2048.0 - top / hz.max(1e-6)), 0.0, 2047.0)
}

pub fn pulse(hz: f64, n: usize, sr: f64, duty: i32, sweep: Option<(i32, i32, bool)>) -> Vec<f32> {
    let mut x = vec![period(hz, 131072.0); n];
    if let Some((pace, shift, down)) = sweep {
        let step = (util::py_round(sr * pace as f64 / 128.0).max(1)) as usize;
        let mut cur = x[0];
        let mut i = 0;
        while i < n {
            let end = (i + step).min(n);
            for s in &mut x[i..end] {
                *s = cur;
            }
            let delta = (cur / 2.0f64.powi(shift)).floor();
            cur = if down { (cur - delta).max(0.0) } else { (cur + delta).min(2047.0) };
            i += step;
        }
    }
    let width = [1, 2, 4, 6][(duty & 3) as usize];
    let mut cum = 0.0f64;
    x.iter()
        .map(|&xv| {
            cum += 131072.0 / (2048.0 - xv);
            let pos = ((cum / sr * 8.0).floor() as i64).rem_euclid(8);
            if pos < width {
                1.0
            } else {
                0.0
            }
        })
        .collect()
}

pub fn wave(hz: f64, n: usize, sr: f64, table: &[u8]) -> Vec<f32> {
    let x = period(hz, 65536.0);
    let real = 65536.0 / (2048.0 - x);
    (0..n)
        .map(|i| {
            let idx = ((i as f64 / sr * real * 32.0).floor() as i64).rem_euclid(32) as usize;
            table[idx] as f32
        })
        .collect()
}

pub fn noise(n: usize, sr: f64, divider: i32, shift: i32, short: bool) -> Vec<f32> {
    let div = if divider == 0 { 0.5 } else { divider as f64 };
    let rate = 262144.0 / (div * 2.0f64.powi(shift.clamp(0, 13)));
    let bits = lfsr_bits(short);
    (0..n)
        .map(|i| bits[((i as f64 / sr * rate) as i64).rem_euclid(bits.len() as i64) as usize])
        .collect()
}

pub fn envelope(n: usize, sr: f64, start: i32, pace: i32, up: bool) -> Vec<f32> {
    if pace <= 0 {
        return vec![start as f32; n];
    }
    (0..n)
        .map(|i| {
            let steps = (i as f64 / sr * 64.0 / pace as f64).floor();
            let vol = if up { start as f64 + steps } else { start as f64 - steps };
            np::clip(vol, 0.0, 15.0) as f32
        })
        .collect()
}

pub fn down(y: &[f32], sr: u32) -> Vec<f32> {
    let isr = (sr as usize * OVERSAMPLE) as f64;
    let lp = util::lowpass8_f32(y, 0.45 * sr as f64, isr, FftLen::Plus(4096));
    lp.iter().step_by(OVERSAMPLE).map(|v| *v as f32).collect()
}

pub fn render_note(voice: &Voice, hz: f64, dur: f64, sr: u32, level: f64) -> Vec<f32> {
    let n = (util::py_round(dur * sr as f64).max(2)) as usize;
    let hz = if voice.base_hz != 0.0 { voice.base_hz } else { hz };
    let isr = (sr as usize * OVERSAMPLE) as f64;
    let m = n * OVERSAMPLE;

    let start = np::clip(np::round_even(voice.volume as f64 * level.max(1.0 / 15.0)), 1.0, 15.0) as i32;
    let digital: Vec<f32> = if voice.kind == Kind::Wave {
        let shift = match voice.level {
            1 => 0,
            2 => 1,
            3 => 2,
            _ => return vec![0.0; n],
        };
        let div = 2.0f32.powi(shift);
        let mut d: Vec<f32> = wave(hz, m, isr, voice.table).iter().map(|x| (*x / div).floor()).collect();
        if level < 0.6 {
            for x in &mut d {
                *x = (*x / 2.0).floor();
            }
        }
        d
    } else {
        let raw = if voice.kind == Kind::Pulse {
            pulse(hz, m, isr, voice.duty, voice.sweep)
        } else {
            noise(m, isr, voice.divider, voice.shift, voice.short)
        };
        let env = envelope(m, isr, start, voice.pace, false);
        raw.iter().zip(env.iter()).map(|(a, b)| a * b).collect()
    };

    let analog: Vec<f32> = dac(&digital).iter().map(|x| *x - 1.0f32).collect();
    let mut out = capacitor(&down(&analog, sr), sr);
    let (g, a, d) = (f32c(voice.gain), f32c(0.62), f32c(2.0));
    for o in &mut out {
        *o = *o * g * a / d;
    }
    let edge = n.min((0.003 * sr as f64) as usize);
    util::fade_tail_f32(&mut out, edge);
    out
}

/// A recording as a Game Boy had to play one: 4 bits through the wave table.
pub fn pcm(y: &[f32], sr: u32) -> Vec<f32> {
    pcm_rate(y, sr, 8192.0)
}

pub fn pcm_rate(y: &[f32], sr: u32, rate: f64) -> Vec<f32> {
    if y.is_empty() {
        return Vec::new();
    }
    let step = (util::py_round(sr as f64 / rate).max(1)) as usize;
    let held = util::hold_samples(y, step);
    let peak = np::abs_max_f32(&held);
    let peak = if peak == 0.0 { 1.0 } else { peak };
    let seven_five = f32c(7.5);
    let digital: Vec<f32> = held
        .iter()
        .map(|h| ((1.0f32 - *h / peak) * seven_five).round_ties_even().clamp(0.0, 15.0))
        .collect();
    let analog: Vec<f32> = dac(&digital).iter().map(|x| *x - 1.0f32).collect();
    capacitor(&analog, sr).iter().map(|x| *x * peak).collect()
}

pub fn voice_for(role: &str, _program: Option<u8>) -> &'static Voice {
    let head = role.split(':').next().unwrap_or("");
    if head == "drums" {
        let group = role.split(':').next_back().unwrap_or("");
        let name = DRUMS.iter().find(|(g, _)| *g == group).map(|(_, n)| *n).unwrap_or("gb-perc");
        return voice(name).unwrap();
    }
    let name = match head {
        "bass" => "gb-wave",
        "lead" => "gb-pulse",
        "chords" => "gb-pulse-full",
        "rhythm" => "gb-pulse-thin",
        _ => "gb-pulse",
    };
    voice(name).unwrap()
}
