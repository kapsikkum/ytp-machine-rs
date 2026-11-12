//! The NES / Famicom 2A03 (port of `app/ytpmv/nes.py`): two pulses, the
//! triangle, the noise register and the DMC, generated at 4x and filtered down.

use std::sync::OnceLock;

use crate::np;
use crate::util::{self, f32c, FftLen};

pub const CPU: f64 = 1789773.0;
pub const NOISE_PERIODS: [i32; 16] = [4, 8, 16, 32, 64, 96, 128, 160, 202, 254, 380, 508, 762, 1016, 2034, 4068];
pub const DMC_PERIODS: [i32; 16] = [428, 380, 340, 320, 286, 254, 226, 214, 190, 160, 142, 128, 106, 84, 72, 54];
pub const OVERSAMPLE: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Pulse,
    Triangle,
    Noise,
}

#[derive(Clone, Copy, Debug)]
pub struct Voice {
    pub name: &'static str,
    pub about: &'static str,
    pub kind: Kind,
    pub duty: i32,
    pub setting: i32,
    pub mode: i32,
    pub decay: f64,
    pub bend: f64,
    pub bend_time: f64,
    pub base_hz: f64,
    pub transpose: i32,
    pub gain: f64,
}

const fn v(name: &'static str, about: &'static str, kind: Kind) -> Voice {
    Voice {
        name,
        about,
        kind,
        duty: 2,
        setting: 8,
        mode: 0,
        decay: 0.0,
        bend: 0.0,
        bend_time: 0.01,
        base_hz: 0.0,
        transpose: 0,
        gain: 1.0,
    }
}

pub static VOICES: &[Voice] = &[
    Voice { duty: 1, ..v("pulse", "a quarter-width square, the NES lead", Kind::Pulse) },
    Voice { duty: 0, ..v("pulse-thin", "an eighth-width square, thin and reedy", Kind::Pulse) },
    Voice { duty: 2, ..v("pulse-full", "a half-width square, round and hollow", Kind::Pulse) },
    v("triangle", "the triangle bass, on or off and nothing between", Kind::Triangle),
    Voice {
        duty: 2,
        decay: 0.10,
        bend: 26.0,
        bend_time: 0.013,
        base_hz: 62.0,
        ..v("nes-kick", "a pulse dropped hard, the NES kick", Kind::Pulse)
    },
    Voice { setting: 5, decay: 0.13, ..v("nes-snare", "noise, cut short", Kind::Noise) },
    Voice { setting: 1, decay: 0.035, ..v("nes-hat", "noise, cut very short", Kind::Noise) },
    Voice {
        duty: 2,
        decay: 0.16,
        bend: 14.0,
        bend_time: 0.03,
        base_hz: 110.0,
        ..v("nes-tom", "a pulse dropped less hard", Kind::Pulse)
    },
    Voice { setting: 0, decay: 0.45, ..v("nes-cymbal", "noise, left to ring", Kind::Noise) },
    Voice {
        setting: 3,
        mode: 1,
        decay: 0.05,
        ..v("nes-perc", "a tick off the short shift register", Kind::Noise)
    },
];

pub const DRUMS: &[(&str, &str)] = &[
    ("kick", "nes-kick"),
    ("snare", "nes-snare"),
    ("hats", "nes-hat"),
    ("toms", "nes-tom"),
    ("cymbals", "nes-cymbal"),
    ("perc", "nes-perc"),
];

pub fn voice(name: &str) -> Option<&'static Voice> {
    VOICES.iter().find(|v| v.name == name)
}

/// The shift register's output over one full cycle.
pub fn noise_bits(mode: i32) -> &'static [f32] {
    static LONG: OnceLock<Vec<f32>> = OnceLock::new();
    static SHORT: OnceLock<Vec<f32>> = OnceLock::new();
    let cell = if mode != 0 { &SHORT } else { &LONG };
    cell.get_or_init(|| {
        let tap = if mode != 0 { 6 } else { 1 };
        let mut reg: u32 = 1;
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        while seen.insert(reg) {
            out.push(((!reg) & 1) as f32);
            reg = (reg >> 1) | (((reg ^ (reg >> tap)) & 1) << 14);
        }
        out
    })
}

/// The 11-bit divider the chip would be set to for *hz*.
pub fn timer(hz: f64, div: f64) -> i64 {
    if hz <= 0.0 {
        return 2047;
    }
    np::clip(np::round_even(CPU / (div * hz) - 1.0), 0.0, 2047.0) as i64
}

pub fn pulse(hz: f64, n: usize, duty: i32, sr: f64, bend: f64, bend_time: f64) -> Vec<f32> {
    let width = [1, 2, 4, 2][(duty & 3) as usize];
    let invert = duty & 3 == 3;
    let mut cum = 0.0f64;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let f = if bend != 0.0 {
            let t = i as f64 / sr;
            hz * 2.0f64.powf(bend * (-t / bend_time).exp() / 12.0)
        } else {
            hz
        };
        let div = np::clip(np::round_even(CPU / (16.0 * f.max(1e-6)) - 1.0), 8.0, 2047.0);
        let real = CPU / (16.0 * (div + 1.0));
        cum += real;
        let step = ((cum / sr * 8.0).floor() as i64).rem_euclid(8);
        let mut on = step < width;
        if invert {
            on = !on;
        }
        out.push(if on { 15.0 } else { 0.0 });
    }
    out
}

pub fn triangle(hz: f64, n: usize, sr: f64) -> Vec<f32> {
    let t = timer(hz, 32.0);
    let real = CPU / (32.0 * (t as f64 + 1.0));
    (0..n)
        .map(|i| {
            let step = ((i as f64 / sr * real * 32.0).floor() as i64).rem_euclid(32);
            if step < 16 {
                (15 - step) as f32
            } else {
                (step - 16) as f32
            }
        })
        .collect()
}

pub fn noise(setting: i32, n: usize, sr: f64, mode: i32) -> Vec<f32> {
    let bits = noise_bits(mode);
    let rate = CPU / NOISE_PERIODS[setting.clamp(0, 15) as usize] as f64;
    let mul = f32c(15.0);
    (0..n)
        .map(|i| {
            let idx = ((i as f64 / sr * rate) as i64).rem_euclid(bits.len() as i64) as usize;
            bits[idx] * mul
        })
        .collect()
}

/// A sample as the DMC would play it back: seven bits, steps of two.
pub fn dpcm(y: &[f32], setting: i32, sr: u32) -> Vec<f32> {
    if y.is_empty() {
        return Vec::new();
    }
    let sr = sr as f64;
    let rate = CPU / DMC_PERIODS[setting.clamp(0, 15) as usize] as f64;
    let steps = ((y.len() as f64 / sr * rate) as usize).max(1);
    let xp: Vec<f64> = (0..y.len()).map(|i| i as f64 / sr).collect();
    let x: Vec<f64> = (0..steps).map(|i| i as f64 / rate).collect();
    let yd: Vec<f64> = y.iter().map(|v| *v as f64).collect();
    let want = np::interp(&x, &xp, &yd);
    let mut out = Vec::with_capacity(steps);
    let mut level = 64.0f64;
    for w in want {
        let w = np::clip(w * 63.0 + 64.0, 0.0, 127.0);
        level = if w > level { (level + 2.0).min(127.0) } else { (level - 2.0).max(0.0) };
        out.push(level as f32);
    }
    (0..y.len())
        .map(|i| {
            let k = ((i as f64 / sr * rate) as usize).min(steps - 1);
            out[k]
        })
        .collect()
}

/// `_down`: lowpass at 0.45 of the output rate and keep every 4th sample.
pub fn down(y: &[f32], sr: u32) -> Vec<f32> {
    let isr = (sr as usize * OVERSAMPLE) as f64;
    let lp = util::lowpass8_f32(y, 0.45 * sr as f64, isr, FftLen::Double);
    lp.iter().step_by(OVERSAMPLE).map(|v| *v as f32).collect()
}

pub fn mix_pulse(v: &[f32]) -> Vec<f32> {
    let (a, b, c) = (f32c(95.88), f32c(8128.0), f32c(100.0));
    let floor = f32c(1e-9);
    v.iter().map(|x| a / (b / x.max(floor) + c)).collect()
}

/// The triangle, noise and DMC share a curve. Evaluated in float64, as numpy
/// does once the 0-d `np.asarray(0.0)` arguments join the float32 one.
pub fn mix_tnd(tri: Option<&[f32]>, noi: Option<&[f32]>) -> Vec<f32> {
    let n = tri.map(|t| t.len()).or(noi.map(|t| t.len())).unwrap_or(0);
    (0..n)
        .map(|i| {
            let t = tri.map(|t| t[i] as f64).unwrap_or(0.0);
            let o = noi.map(|t| t[i] as f64).unwrap_or(0.0);
            let inner = t / 8227.0 + o / 12241.0 + 0.0 / 22638.0;
            (159.79 / (1.0 / inner.max(1e-12) + 100.0)) as f32
        })
        .collect()
}

pub fn floor_hz(voice: &Voice) -> f64 {
    if voice.base_hz != 0.0 || voice.kind == Kind::Noise {
        return 0.0;
    }
    CPU / ((if voice.kind == Kind::Triangle { 32.0 } else { 16.0 }) * 2048.0)
}

pub fn render_note(voice: &Voice, hz: f64, dur: f64, sr: u32, level: f64) -> Vec<f32> {
    let n = (util::py_round(dur * sr as f64).max(2)) as usize;
    let base = if voice.base_hz != 0.0 { voice.base_hz } else { hz };
    let hz = base * 2.0f64.powf(voice.transpose as f64 / 12.0);
    let isr = (sr as usize * OVERSAMPLE) as f64;
    let m = n * OVERSAMPLE;

    let raw = match voice.kind {
        Kind::Triangle => mix_tnd(Some(&triangle(hz, m, isr)), None),
        Kind::Noise => mix_tnd(None, Some(&noise(voice.setting, m, isr, voice.mode))),
        Kind::Pulse => mix_pulse(&pulse(hz, m, voice.duty, isr, voice.bend, voice.bend_time)),
    };
    let mut out = down(&raw, sr);
    let mean = np::mean_f32(&out);
    for o in &mut out {
        *o -= mean;
    }
    let peak = np::abs_max_f32(&out);
    if peak > 0.0 {
        let (p, a, g) = (peak, f32c(0.62), f32c(voice.gain));
        for o in &mut out {
            *o = *o / p * a * g;
        }
    }

    let sr_f = sr as f64;
    let edge = n.min((0.002 * sr_f) as usize);
    if voice.kind != Kind::Triangle {
        // float32 * float64 envelope promotes to float64 from here on
        let mut wide: Vec<f64> = out.iter().map(|v| *v as f64).collect();
        if voice.decay > 0.0 {
            for (i, w) in wide.iter_mut().enumerate() {
                let t = i as f64 / sr_f;
                *w *= (1.0 - t / voice.decay).max(0.0).powf(1.5);
            }
        }
        let vel = (util::py_round(level * 15.0).max(1)) as f64 / 15.0;
        if voice.decay > 0.0 {
            for w in &mut wide {
                *w *= vel;
            }
            util::fade_tail_f64_by_f32(&mut wide, edge);
            return wide.iter().map(|v| *v as f32).collect();
        }
        // no decay: stays float32 and the velocity is a float32 scalar
        let vf = f32c(vel);
        for o in &mut out {
            *o *= vf;
        }
    }
    util::fade_tail_f32(&mut out, edge);
    out
}

pub fn voice_for(role: &str, program: Option<u8>) -> &'static Voice {
    let head = role.split(':').next().unwrap_or("");
    if head == "drums" {
        let group = role.split(':').next_back().unwrap_or("");
        let name = DRUMS.iter().find(|(g, _)| *g == group).map(|(_, n)| *n).unwrap_or("nes-perc");
        return voice(name).unwrap();
    }
    let by_role = match head {
        "bass" => Some("triangle"),
        "lead" => Some("pulse"),
        "chords" => Some("pulse-full"),
        "rhythm" => Some("pulse-thin"),
        _ => None,
    };
    let name = by_role.or_else(|| {
        program.and_then(|p| match p {
            0..=31 => Some("pulse-full"),
            32..=39 => Some("triangle"),
            40..=103 => Some("pulse"),
            104..=127 => Some("pulse-thin"),
            _ => None,
        })
    });
    voice(name.unwrap_or("pulse")).unwrap()
}
