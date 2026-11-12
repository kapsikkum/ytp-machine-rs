//! The Commodore 64 SID, 6581 and 8580 (port of `app/ytpmv/sid.py`).
//!
//! 24-bit phase accumulator oscillators, reSID's ADSR counter, the R-2R DAC
//! ladders, both measured filter curves, and the C64 board's output filter.

use std::f64::consts::PI;
use std::sync::OnceLock;

use crate::gb::cdiv;
use crate::np::{self, C64};
use crate::util::{self, f32c};

pub const CLOCK: f64 = 985248.0;
pub const OVERSAMPLE: usize = 4;
pub const RATE_PERIOD: [i64; 16] = [9, 32, 63, 95, 149, 220, 267, 313, 392, 977, 1954, 3126, 3907, 11720, 19532, 31251];
const EXP_STEPS: [(i64, i64); 6] = [(0x5D, 1), (0x36, 2), (0x1A, 4), (0x0E, 8), (0x06, 16), (0x00, 30)];

pub static F0_6581: [(f64, f64); 27] = [
    (0.0, 220.0), (128.0, 230.0), (256.0, 250.0), (384.0, 300.0), (512.0, 420.0), (640.0, 780.0),
    (768.0, 1600.0), (832.0, 2300.0), (896.0, 3200.0), (960.0, 4300.0), (992.0, 5000.0),
    (1008.0, 5400.0), (1016.0, 5700.0), (1023.0, 6000.0), (1024.0, 4600.0), (1032.0, 4800.0),
    (1056.0, 5300.0), (1088.0, 6000.0), (1120.0, 6600.0), (1152.0, 7200.0), (1280.0, 9500.0),
    (1408.0, 12000.0), (1536.0, 14500.0), (1664.0, 16000.0), (1792.0, 17100.0),
    (1920.0, 17700.0), (2047.0, 18000.0),
];
pub static F0_8580: [(f64, f64); 17] = [
    (0.0, 0.0), (128.0, 800.0), (256.0, 1600.0), (384.0, 2500.0), (512.0, 3300.0), (640.0, 4100.0),
    (768.0, 4800.0), (896.0, 5600.0), (1024.0, 6500.0), (1152.0, 7500.0), (1280.0, 8400.0),
    (1408.0, 9200.0), (1536.0, 9800.0), (1664.0, 10500.0), (1792.0, 11000.0),
    (1920.0, 11700.0), (2047.0, 12500.0),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Model {
    #[default]
    Mos6581,
    Mos8580,
}

impl Model {
    pub fn parse(s: &str) -> Self {
        if s == "6581" {
            Model::Mos6581
        } else {
            Model::Mos8580
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wave {
    Tri,
    Saw,
    Pulse,
    Noise,
}

#[derive(Clone, Copy, Debug)]
pub struct Voice {
    pub name: &'static str,
    pub about: &'static str,
    pub wave: Wave,
    pub pw: i32,
    pub pwm: Option<(f64, f64)>,
    pub adsr: (i32, i32, i32, i32),
    pub filt: &'static str,
    pub fc: i32,
    pub fc_end: Option<i32>,
    pub sweep_time: f64,
    pub res: i32,
    pub ring: f64,
    pub drop: Option<(f64, f64)>,
    pub base_hz: f64,
    pub gain: f64,
}

const fn v(name: &'static str, about: &'static str) -> Voice {
    Voice {
        name,
        about,
        wave: Wave::Pulse,
        pw: 0x800,
        pwm: None,
        adsr: (0, 9, 10, 6),
        filt: "",
        fc: 1024,
        fc_end: None,
        sweep_time: 0.3,
        res: 8,
        ring: 0.0,
        drop: None,
        base_hz: 0.0,
        gain: 1.0,
    }
}

pub static VOICES: &[Voice] = &[
    Voice {
        pw: 0x600,
        pwm: Some((4.5, 900.0)),
        adsr: (0, 8, 11, 6),
        ..v("sid-lead", "a pulse with its width swept, the C64 lead")
    },
    Voice {
        wave: Wave::Saw,
        adsr: (0, 7, 10, 5),
        filt: "lp",
        fc: 1500,
        fc_end: Some(380),
        sweep_time: 0.18,
        res: 11,
        ..v("sid-bass", "a sawtooth through the filter as it closes")
    },
    Voice {
        pw: 0x300,
        pwm: Some((0.8, 400.0)),
        adsr: (6, 9, 12, 8),
        gain: 0.9,
        ..v("sid-pad", "a narrow pulse, softly attacked")
    },
    Voice { pw: 0x400, adsr: (0, 6, 4, 4), ..v("sid-rhythm", "a pulse cut short") },
    Voice {
        wave: Wave::Tri,
        ring: 2.0,
        adsr: (0, 10, 0, 9),
        ..v("sid-bell", "a triangle ring-modulated, bell-like and in tune")
    },
    Voice {
        wave: Wave::Tri,
        adsr: (0, 5, 0, 5),
        drop: Some((28.0, 0.02)),
        base_hz: 55.0,
        gain: 1.4,
        ..v("sid-kick", "a triangle dropped fast")
    },
    Voice {
        wave: Wave::Noise,
        adsr: (0, 6, 0, 6),
        filt: "hp+bp",
        fc: 780,
        res: 4,
        base_hz: 2800.0,
        ..v("sid-snare", "noise, high-passed a little and cut short")
    },
    Voice {
        wave: Wave::Noise,
        adsr: (0, 3, 0, 3),
        filt: "hp",
        fc: 1060,
        res: 2,
        base_hz: 6000.0,
        gain: 0.9,
        ..v("sid-hat", "noise, high-passed hard")
    },
    Voice {
        wave: Wave::Tri,
        adsr: (0, 7, 0, 7),
        drop: Some((12.0, 0.04)),
        base_hz: 120.0,
        gain: 1.2,
        ..v("sid-tom", "a triangle dropped less hard")
    },
    Voice {
        wave: Wave::Noise,
        adsr: (0, 10, 0, 10),
        filt: "hp",
        fc: 1000,
        res: 3,
        base_hz: 5000.0,
        gain: 0.9,
        ..v("sid-cymbal", "noise, left to ring")
    },
    Voice {
        pw: 0x200,
        adsr: (0, 2, 0, 2),
        base_hz: 900.0,
        gain: 0.8,
        ..v("sid-perc", "a pulse ticked")
    },
];

pub const DRUMS: &[(&str, &str)] = &[
    ("kick", "sid-kick"),
    ("snare", "sid-snare"),
    ("hats", "sid-hat"),
    ("toms", "sid-tom"),
    ("cymbals", "sid-cymbal"),
    ("perc", "sid-perc"),
];

pub fn voice(name: &str) -> Option<&'static Voice> {
    VOICES.iter().find(|v| v.name == name)
}

fn compute_dac(model: Model) -> Vec<f64> {
    let bits = 12usize;
    let (ratio, term) = if model == Model::Mos6581 { (2.20, false) } else { (2.00, true) };
    let mut vbit = Vec::new();
    for set_bit in 0..bits {
        let (mut vn, r, r2) = (1.0f64, 1.0f64, ratio);
        let mut rn = if term { r2 } else { f64::INFINITY };
        for _ in 0..set_bit {
            rn = if rn == f64::INFINITY { r + r2 } else { r + r2 * rn / (r2 + rn) };
        }
        if rn == f64::INFINITY {
            rn = r2;
        } else {
            rn = r2 * rn / (r2 + rn);
            vn = vn * rn / r2;
        }
        for _ in (set_bit + 1)..bits {
            rn += r;
            let current = vn / rn;
            rn = r2 * rn / (r2 + rn);
            vn = rn * current;
        }
        vbit.push(vn);
    }
    let mut table = vec![0.0f64; 1 << bits];
    for (code, t) in table.iter_mut().enumerate() {
        let mut acc = 0.0f64;
        for (j, vb) in vbit.iter().enumerate() {
            acc += ((code >> j) & 1) as f64 * vb;
        }
        *t = acc;
    }
    let max = table.iter().cloned().fold(f64::MIN, f64::max);
    table.iter().map(|x| x / max).collect()
}

/// The twelve-bit R-2R ladder, as reSID computes it for each chip.
pub fn dac_table(model: Model) -> &'static [f64] {
    static T6581: OnceLock<Vec<f64>> = OnceLock::new();
    static T8580: OnceLock<Vec<f64>> = OnceLock::new();
    match model {
        Model::Mos6581 => T6581.get_or_init(|| compute_dac(Model::Mos6581)),
        Model::Mos8580 => T8580.get_or_init(|| compute_dac(Model::Mos8580)),
    }
}

pub fn cutoff_hz(fc: f64, model: Model) -> f64 {
    let pts: &[(f64, f64)] = if model == Model::Mos6581 { &F0_6581 } else { &F0_8580 };
    let xs: Vec<f64> = pts.iter().map(|p| p.0).collect();
    let ys: Vec<f64> = pts.iter().map(|p| p.1).collect();
    let fc = np::clip(fc, 0.0, 2047.0);
    np::interp(&[fc], &xs, &ys)[0]
}

pub fn inv_q(res: i32, model: Model) -> f64 {
    let res = res.clamp(0, 15);
    if model == Model::Mos6581 {
        ((15 - res) as f64 / 8.0).max(0.12)
    } else {
        2.0f64.powf((4 - res) as f64 / 8.0)
    }
}

fn noise_bits() -> &'static [u8] {
    static BITS: OnceLock<Vec<u8>> = OnceLock::new();
    BITS.get_or_init(|| {
        let n = 1usize << 20;
        let mut b = vec![0u8; n + 23];
        for (i, slot) in b.iter_mut().enumerate().take(23) {
            *slot = ((0x7FFFF8u32 >> (22 - i)) & 1) as u8;
        }
        for i in 23..b.len() {
            b[i] = b[i - 23] ^ b[i - 18];
        }
        b
    })
}

fn noise_wave(steps: &[i64]) -> Vec<i64> {
    let b = noise_bits();
    let m = (b.len() - 23) as i64;
    let taps: [(usize, u32); 8] = [(20, 11), (18, 10), (14, 9), (11, 8), (9, 7), (5, 6), (2, 5), (0, 4)];
    steps
        .iter()
        .map(|s| {
            let k = s.rem_euclid(m) as usize + 23;
            let mut out = 0i64;
            for (reg_bit, out_bit) in taps {
                out |= (b[k - reg_bit] as i64) << out_bit;
            }
            out
        })
        .collect()
}

fn fall_times(start: i64, stop: i64, rate: i32) -> Vec<f64> {
    let mut out = vec![0.0f64];
    let mut acc = 0.0f64;
    let mut lvl = start;
    while lvl > stop {
        let mult = if lvl > 0 {
            EXP_STEPS.iter().find(|(thr, _)| lvl > *thr).map(|(_, m)| *m).unwrap_or(30)
        } else {
            30
        };
        acc += (RATE_PERIOD[(rate & 15) as usize] * mult) as f64;
        out.push(acc);
        lvl -= 1;
    }
    out
}

/// `np.searchsorted(a, v, side="right")`
fn search_right(a: &[f64], v: f64) -> usize {
    a.partition_point(|x| *x <= v)
}

/// The ADSR counter over time, 0..255, as reSID steps it.
pub fn envelope(n: usize, sr: f64, adsr: (i32, i32, i32, i32), gate_samples: usize, clock: f64) -> Vec<f64> {
    let (attack, decay, sustain, release) = adsr;
    let ap = RATE_PERIOD[(attack & 15) as usize];
    let a_len = (255 * ap) as f64;
    let sus = 0x11 * (sustain & 15) as i64;
    let dt = fall_times(255, sus, decay);
    let t: Vec<f64> = (0..n).map(|i| i as f64 / sr * clock).collect();
    let mut env: Vec<f64> = t
        .iter()
        .map(|&tv| {
            if tv < a_len {
                (tv / ap as f64).min(255.0)
            } else {
                let since = (tv - a_len).max(0.0);
                let held = 255 - search_right(&dt, since) as i64 + 1;
                held.max(sus) as f64
            }
        })
        .collect();

    let off = gate_samples as f64 / sr * clock;
    if gate_samples < n {
        let at = env[gate_samples.min(n - 1)];
        let rt = fall_times(at as i64, 0, release);
        for (e, tv) in env.iter_mut().zip(t.iter()) {
            if *tv >= off {
                let since_off = (tv - off).max(0.0);
                let rel = at as i64 - search_right(&rt, since_off) as i64 + 1;
                *e = rel.max(0) as f64;
            }
        }
    }
    env
}

fn svf_response(freqs: &[f64], sr: f64, fc: f64, k: f64, mode: &str) -> Vec<C64> {
    let g = (PI * fc.min(0.49 * sr) / sr).tan();
    let one = C64::new(1.0, 0.0);
    freqs
        .iter()
        .map(|&f| {
            let th = 2.0 * PI * f / sr;
            let z = C64::new(th.cos(), th.sin());
            let s = cdiv(cdiv(z - one, z + one), C64::new(g, 0.0));
            let den = s * s + s * k + one;
            let mut acc = C64::new(0.0, 0.0);
            for m in mode.split('+') {
                acc += match m {
                    "lp" => cdiv(one, den),
                    "bp" => cdiv(s, den),
                    _ => cdiv(s * s, den),
                };
            }
            acc
        })
        .collect()
}

fn filter_static(y: &[f64], sr: f64, fc: f64, k: f64, mode: &str) -> Vec<f64> {
    let n = util::pow2_ceil(y.len() + 8192);
    let f = np::rfftfreq(n, 1.0 / sr);
    let resp = svf_response(&f, sr, fc, k, mode);
    let mut spec = np::rfft(y, n);
    for (s, r) in spec.iter_mut().zip(resp.iter()) {
        *s *= *r;
    }
    let mut out = np::irfft(&spec, n);
    out.truncate(y.len());
    out
}

fn filter_sweep(y: &[f64], sr: f64, fcs: &[f64], k: f64, mode: &str) -> Vec<f64> {
    let (lp_on, bp_on, hp_on) = (mode.contains("lp"), mode.contains("bp"), mode.contains("hp"));
    let mut out = Vec::with_capacity(y.len());
    let (mut ic1, mut ic2) = (0.0f64, 0.0f64);
    let mut last_fc = f64::NAN;
    let mut g = 0.0f64;
    for (i, &yi) in y.iter().enumerate() {
        if fcs[i] != last_fc {
            last_fc = fcs[i];
            g = (PI * last_fc.min(0.49 * sr) / sr).tan();
        }
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let a3 = g * a2;
        let v3 = yi - ic2;
        let v1 = a1 * ic1 + a2 * v3;
        let v2 = ic2 + a2 * ic1 + a3 * v3;
        ic1 = 2.0 * v1 - ic1;
        ic2 = 2.0 * v2 - ic2;
        let lp = if lp_on { v2 } else { 0.0 };
        let bp = if bp_on { v1 } else { 0.0 };
        let hp = if hp_on { yi - k * v1 - v2 } else { 0.0 };
        out.push(lp + bp + hp);
    }
    out
}

fn board_response(f: f64) -> C64 {
    let one = C64::new(1.0, 0.0);
    let a = one + C64::new(0.0, f * (1.0 / 16000.0));
    let b = C64::new(0.0, f * (1.0 / 16.0));
    let c = one + C64::new(0.0, f * (1.0 / 16.0));
    cdiv(cdiv(one, a) * b, c)
}

fn board_spec(spec: &mut [C64], n: usize, sr: f64) {
    let f = np::rfftfreq(n, 1.0 / sr);
    for (s, fr) in spec.iter_mut().zip(f.iter()) {
        *s *= board_response(*fr);
    }
}

/// The C64's output: 16 kHz low-pass, 16 Hz high-pass.
fn board(y: &[f64], sr: f64) -> Vec<f64> {
    let n = util::pow2_ceil(y.len() + 8192);
    let mut spec = np::rfft(y, n);
    board_spec(&mut spec, n, sr);
    let mut out = np::irfft(&spec, n);
    out.truncate(y.len());
    out
}

fn board_f32(y: &[f32], sr: f64) -> Vec<f64> {
    let n = util::pow2_ceil(y.len() + 8192);
    let mut spec = np::rfft_f32(y, n);
    board_spec(&mut spec, n, sr);
    let mut out = np::irfft(&spec, n);
    out.truncate(y.len());
    out
}

pub fn render_note(voice: &Voice, hz: f64, dur: f64, sr: u32, level: f64, model: Model, tail: f64) -> Vec<f32> {
    let hz = if voice.base_hz != 0.0 { voice.base_hz } else { hz };
    let sr_f = sr as f64;
    let isr = sr_f * OVERSAMPLE as f64;
    let gate = (util::py_round(dur * isr).max(2)) as usize;
    let n = gate + (tail * isr) as usize;
    let t: Vec<f64> = (0..n).map(|i| i as f64 / isr).collect();

    let mut inc = Vec::with_capacity(n);
    for &tv in &t {
        let f = match voice.drop {
            Some((semis, secs)) => hz * 2.0f64.powf(semis * (-tv / secs.max(1e-4)).exp() / 12.0),
            None => hz,
        };
        let fn_ = np::clip(np::round_even(f * 16777216.0 / CLOCK), 0.0, 65535.0);
        inc.push(fn_ * CLOCK / isr);
    }
    let two24 = 16777216.0f64;
    let mut cum = 0.0f64;
    let acc: Vec<i64> = inc
        .iter()
        .map(|&d| {
            cum += d;
            (cum % two24) as i64
        })
        .collect();

    let wave: Vec<i64> = match voice.wave {
        Wave::Saw => acc.iter().map(|a| (a >> 12) & 0xFFF).collect(),
        Wave::Tri => {
            let mut folded: Vec<i64> =
                acc.iter().map(|&a| if (a >> 23) & 1 == 1 { !a } else { a }).collect();
            if voice.ring != 0.0 {
                let mut pc = 0.0f64;
                for (f, &d) in folded.iter_mut().zip(inc.iter()) {
                    pc += d * voice.ring;
                    let partner = (pc % two24) as i64;
                    if (partner >> 23) & 1 == 1 {
                        *f = !*f;
                    }
                }
            }
            folded.iter().map(|f| (f >> 11) & 0xFFF).collect()
        }
        Wave::Noise => {
            let mut c = 0.0f64;
            let steps: Vec<i64> = inc
                .iter()
                .map(|&d| {
                    c += d;
                    (c / 1048576.0).floor() as i64
                })
                .collect();
            noise_wave(&steps)
        }
        Wave::Pulse => acc
            .iter()
            .enumerate()
            .map(|(i, &a)| {
                let top = (a >> 12) & 0xFFF;
                let mut pw = voice.pw as f64;
                if let Some((rate, depth)) = voice.pwm {
                    pw += depth * (2.0 * PI * rate * t[i]).sin();
                }
                if top as f64 >= np::clip(pw, 0.0, 4095.0) {
                    0xFFF
                } else {
                    0
                }
            })
            .collect(),
    };

    let dac = dac_table(model);
    let env = envelope(n, isr, voice.adsr, gate, CLOCK);
    let mut y: Vec<f64> = wave
        .iter()
        .zip(env.iter())
        .map(|(&w, &e)| (dac[w.clamp(0, 4095) as usize] - 0.5) * (e / 255.0))
        .collect();

    // down to the output rate before the filter
    let nfft = util::pow2_ceil(n + 8192);
    let freqs = np::rfftfreq(nfft, 1.0 / isr);
    let mut spec = np::rfft(&y, nfft);
    for (s, f) in spec.iter_mut().zip(freqs.iter()) {
        *s *= 1.0 / (1.0 + (f / (0.45 * sr_f)).powf(8.0));
    }
    y = np::irfft(&spec, nfft);
    y.truncate(n);
    let mut y: Vec<f64> = y.iter().step_by(OVERSAMPLE).copied().collect();
    let m = y.len();

    if !voice.filt.is_empty() {
        let k = inv_q(voice.res, model);
        match voice.fc_end {
            None => y = filter_static(&y, sr_f, cutoff_hz(voice.fc as f64, model), k, voice.filt),
            Some(fc_end) => {
                let reg = |i: usize| {
                    let tt = i as f64 / sr_f;
                    fc_end as f64 + (voice.fc - fc_end) as f64 * (-tt / voice.sweep_time.max(1e-3)).exp()
                };
                let curve: Vec<f64> = (0..m).step_by(64).map(|i| cutoff_hz(reg(i), model)).collect();
                let fcs: Vec<f64> = (0..m).map(|i| curve[i / 64].max(20.0)).collect();
                y = filter_sweep(&y, sr_f, &fcs, k, voice.filt);
            }
        }
    }

    y = board(&y, sr_f);
    for v in &mut y {
        *v *= level;
    }
    let edge = m.min((0.003 * sr_f) as usize);
    util::fade_tail_f64(&mut y, edge);
    y.iter().map(|v| (*v * 1.2 * voice.gain) as f32).collect()
}

/// Speech the C64 way: four bits of PCM hammered into the volume register.
pub fn digi(y: &[f32], sr: u32, model: Model) -> Vec<f32> {
    digi_rate(y, sr, 8000.0, model)
}

pub fn digi_rate(y: &[f32], sr: u32, rate: f64, model: Model) -> Vec<f32> {
    if y.is_empty() {
        return Vec::new();
    }
    let step = (util::py_round(sr as f64 / rate).max(1)) as usize;
    let held = util::hold_samples(y, step);
    let peak = np::abs_max_f32(&held);
    let peak = if peak == 0.0 { 1.0 } else { peak };
    let (one, seven_five) = (f32c(1.0), f32c(7.5));
    let four: Vec<f32> = held
        .iter()
        .map(|h| (((*h / peak + one) * seven_five).round_ties_even() / seven_five - one) * peak)
        .collect();
    let out = board_f32(&four, sr as f64);
    let gain = if model == Model::Mos6581 { 1.0 } else { 0.12 };
    out.iter().map(|v| (*v * gain) as f32).collect()
}

pub fn voice_for(role: &str, program: Option<u8>) -> &'static Voice {
    let head = role.split(':').next().unwrap_or("");
    if head == "drums" {
        let group = role.split(':').next_back().unwrap_or("");
        let name = DRUMS.iter().find(|(g, _)| *g == group).map(|(_, n)| *n).unwrap_or("sid-perc");
        return voice(name).unwrap();
    }
    if let Some(p) = program {
        if (8..=15).contains(&p) && head != "bass" && head != "lead" {
            return voice("sid-bell").unwrap();
        }
    }
    let name = match head {
        "bass" => "sid-bass",
        "lead" => "sid-lead",
        "chords" => "sid-pad",
        "rhythm" => "sid-rhythm",
        _ => "sid-lead",
    };
    voice(name).unwrap()
}
