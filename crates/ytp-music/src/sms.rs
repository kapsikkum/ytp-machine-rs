//! The Master System's SN76489 (port of `app/ytpmv/sms.py`): three squares
//! and a shift-register noise channel with 2 dB volume steps.

use std::sync::OnceLock;

use crate::np;
use crate::util::{self, f32c, FftLen};

pub const CLOCK: f64 = 3579545.0;
pub const OVERSAMPLE: usize = 4;
pub const FLOOR_HZ: f64 = CLOCK / (32.0 * 1023.0);
pub const NOISE_RELOADS: [i32; 3] = [0x10, 0x20, 0x40];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Tone,
    Noise,
}

#[derive(Clone, Copy, Debug)]
pub struct Voice {
    pub name: &'static str,
    pub about: &'static str,
    pub kind: Kind,
    pub volume: i32,
    pub setting: i32,
    pub periodic: bool,
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
        volume: 0,
        setting: 1,
        periodic: false,
        decay: 0.0,
        bend: 0.0,
        bend_time: 0.01,
        base_hz: 0.0,
        transpose: 0,
        gain: 1.0,
    }
}

pub static VOICES: &[Voice] = &[
    v("psg", "a square, which is all this chip has", Kind::Tone),
    Voice { volume: 3, ..v("psg-soft", "the same square, further back", Kind::Tone) },
    Voice { volume: 1, ..v("psg-bass", "a square down where the bass goes", Kind::Tone) },
    Voice { decay: 0.45, ..v("psg-pluck", "a square that fades as it goes", Kind::Tone) },
    Voice {
        decay: 0.09,
        bend: 26.0,
        bend_time: 0.013,
        base_hz: 62.0,
        ..v("sms-kick", "a square dropped hard", Kind::Tone)
    },
    Voice { setting: 2, decay: 0.12, ..v("sms-snare", "white noise, cut short", Kind::Noise) },
    Voice { setting: 0, decay: 0.035, ..v("sms-hat", "white noise, cut very short", Kind::Noise) },
    Voice {
        decay: 0.16,
        bend: 14.0,
        bend_time: 0.03,
        base_hz: 110.0,
        ..v("sms-tom", "a square dropped less hard", Kind::Tone)
    },
    Voice { setting: 2, decay: 0.45, ..v("sms-cymbal", "white noise, left to ring", Kind::Noise) },
    Voice {
        setting: 1,
        periodic: true,
        decay: 0.06,
        ..v("sms-perc", "the register's short mode, which has a pitch", Kind::Noise)
    },
];

pub const DRUMS: &[(&str, &str)] = &[
    ("kick", "sms-kick"),
    ("snare", "sms-snare"),
    ("hats", "sms-hat"),
    ("toms", "sms-tom"),
    ("cymbals", "sms-cymbal"),
    ("perc", "sms-perc"),
];

pub fn voice(name: &str) -> Option<&'static Voice> {
    VOICES.iter().find(|v| v.name == name)
}

/// Volume register *step* as a factor; 15 is off, 2 dB apart.
pub fn attenuation(step: i32) -> f64 {
    let step = step.clamp(0, 15);
    if step >= 15 {
        0.0
    } else {
        10.0f64.powf(-2.0 * step as f64 / 20.0)
    }
}

pub fn lfsr_bits(periodic: bool) -> &'static [f32] {
    static WHITE: OnceLock<Vec<f32>> = OnceLock::new();
    static BUZZ: OnceLock<Vec<f32>> = OnceLock::new();
    if periodic {
        BUZZ.get_or_init(|| {
            let mut v = vec![0.0f32; 16];
            v[0] = 1.0;
            v
        })
    } else {
        WHITE.get_or_init(|| {
            let mut reg: u32 = 0x8000;
            let mut seen = std::collections::HashSet::new();
            let mut out = Vec::new();
            while seen.insert(reg) {
                out.push((reg & 1) as f32);
                let parity = (reg & 0x0009).count_ones() & 1;
                reg = (reg >> 1) | (parity << 15);
            }
            out
        })
    }
}

pub fn down(y: &[f32], sr: u32) -> Vec<f32> {
    let isr = (sr as usize * OVERSAMPLE) as f64;
    let lp = util::lowpass8_f32(y, 0.45 * sr as f64, isr, FftLen::Plus(4096));
    lp.iter().step_by(OVERSAMPLE).map(|v| *v as f32).collect()
}

pub fn divider(hz: f64) -> i64 {
    if hz <= 0.0 {
        return 1023;
    }
    np::clip(np::round_even(CLOCK / (32.0 * hz)), 1.0, 1023.0) as i64
}

pub fn square(hz: f64, n: usize, sr: f64, bend: f64, bend_time: f64) -> Vec<f32> {
    let mut cum = 0.0f64;
    (0..n)
        .map(|i| {
            let f = if bend != 0.0 {
                let t = i as f64 / sr;
                hz * 2.0f64.powf(bend * (-t / bend_time).exp() / 12.0)
            } else {
                hz
            };
            let div = np::clip(np::round_even(CLOCK / (32.0 * f.max(1e-6))), 1.0, 1023.0);
            cum += CLOCK / (32.0 * div);
            let phase = cum / sr;
            if phase % 1.0 < 0.5 {
                1.0
            } else {
                -1.0
            }
        })
        .collect()
}

pub fn noise(setting: i32, n: usize, sr: f64, periodic: bool, tone_hz: f64) -> Vec<f32> {
    let bits = lfsr_bits(periodic);
    let rate = if setting >= 3 && tone_hz > 0.0 {
        CLOCK / (32.0 * divider(tone_hz) as f64)
    } else {
        CLOCK / 16.0 / (2.0 * NOISE_RELOADS[setting.clamp(0, 2) as usize] as f64)
    };
    let (two, one) = (f32c(2.0), f32c(1.0));
    (0..n)
        .map(|i| {
            let idx = ((i as f64 / sr * rate) as i64).rem_euclid(bits.len() as i64) as usize;
            bits[idx] * two - one
        })
        .collect()
}

pub fn floor_hz(voice: &Voice) -> f64 {
    if voice.base_hz != 0.0 || voice.kind == Kind::Noise {
        0.0
    } else {
        FLOOR_HZ
    }
}

pub fn render_note(voice: &Voice, hz: f64, dur: f64, sr: u32, level: f64) -> Vec<f32> {
    let n = (util::py_round(dur * sr as f64).max(2)) as usize;
    let base = if voice.base_hz != 0.0 { voice.base_hz } else { hz };
    let mut hz = base * 2.0f64.powf(voice.transpose as f64 / 12.0);
    if voice.kind == Kind::Tone && voice.base_hz == 0.0 {
        while hz < FLOOR_HZ {
            hz *= 2.0;
        }
    }
    let isr = (sr as usize * OVERSAMPLE) as f64;
    let m = n * OVERSAMPLE;

    let raw = if voice.kind == Kind::Noise {
        noise(voice.setting, m, isr, voice.periodic, hz)
    } else {
        square(hz, m, isr, voice.bend, voice.bend_time)
    };
    let mut out = down(&raw, sr);
    let peak = np::abs_max_f32(&out);
    if peak > 0.0 {
        let (a, g) = (f32c(0.62), f32c(voice.gain));
        for o in &mut out {
            *o = *o / peak * a * g;
        }
    }
    let att = f32c(attenuation(voice.volume));
    for o in &mut out {
        *o *= att;
    }

    let sr_f = sr as f64;
    let edge = n.min((0.002 * sr_f) as usize);
    let step = if level < 1.0 {
        Some(np::clip(np::round_even(-20.0 * level.max(1e-3).log10() / 2.0), 0.0, 15.0) as i32)
    } else {
        None
    };

    if voice.decay > 0.0 {
        let mut wide: Vec<f64> = out.iter().map(|v| *v as f64).collect();
        for (i, w) in wide.iter_mut().enumerate() {
            let t = i as f64 / sr_f;
            *w *= (1.0 - t / voice.decay).max(0.0).powf(1.5);
        }
        if let Some(s) = step {
            let a = attenuation(s);
            for w in &mut wide {
                *w *= a;
            }
        }
        util::fade_tail_f64_by_f32(&mut wide, edge);
        return wide.iter().map(|v| *v as f32).collect();
    }
    if let Some(s) = step {
        let a = f32c(attenuation(s));
        for o in &mut out {
            *o *= a;
        }
    }
    util::fade_tail_f32(&mut out, edge);
    out
}

/// A recording the way this chip had to play one: the volume register hammered.
pub fn pcm(y: &[f32], sr: u32) -> Vec<f32> {
    pcm_rate(y, sr, 11025.0)
}

pub fn pcm_rate(y: &[f32], sr: u32, rate: f64) -> Vec<f32> {
    if y.is_empty() {
        return Vec::new();
    }
    let step = (util::py_round(sr as f64 / rate).max(1)) as usize;
    let held = util::hold_samples(y, step);
    let levels: Vec<f32> = (0..16).map(|s| attenuation(s) as f32).collect();
    held.iter()
        .map(|h| {
            let mag = h.abs();
            let mut best = 0usize;
            let mut best_d = f32::INFINITY;
            for (i, l) in levels.iter().enumerate() {
                let d = (mag - *l).abs();
                if d < best_d {
                    best_d = d;
                    best = i;
                }
            }
            let sign = if *h > 0.0 {
                1.0f32
            } else if *h < 0.0 {
                -1.0
            } else {
                0.0
            };
            sign * levels[best]
        })
        .collect()
}

pub fn voice_for(role: &str, _program: Option<u8>) -> &'static Voice {
    let head = role.split(':').next().unwrap_or("");
    if head == "drums" {
        let group = role.split(':').next_back().unwrap_or("");
        let name = DRUMS.iter().find(|(g, _)| *g == group).map(|(_, n)| *n).unwrap_or("sms-perc");
        return voice(name).unwrap();
    }
    let name = match head {
        "bass" => "psg-bass",
        "lead" => "psg",
        "chords" | "rhythm" => "psg-soft",
        _ => "psg",
    };
    voice(name).unwrap()
}
