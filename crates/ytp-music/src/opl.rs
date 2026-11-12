//! The PC sound card FM chips: OPL2 (YM3812) and OPL3 (YMF262)
//! (port of `app/ytpmv/opl.py`). Patches are Freedoom's GENMIDI lump, embedded.

use std::sync::OnceLock;

use crate::gb::cdiv;
use crate::np::{self, C64};
use crate::util::{self, f32c, FftLen};

pub const RATE: f64 = 49716.0;
pub const MULT: [i64; 16] = [1, 2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 20, 24, 24, 30, 30];
pub const KSL_ROM: [i64; 16] = [0, 32, 40, 45, 48, 51, 53, 55, 56, 58, 59, 60, 61, 62, 63, 64];
pub const KSL_SHIFT: [u32; 4] = [8, 1, 2, 0];

pub static DECAY_PER_SAMPLE: [f64; 64] = [
    0.0, 0.0, 0.0, 0.0, 0.000244, 0.000305, 0.000366, 0.000427, 0.000488, 0.00061, 0.000732, 0.000854,
    0.000977, 0.001221, 0.001465, 0.001709, 0.001953, 0.002441, 0.00293, 0.003418, 0.003906, 0.004883,
    0.005859, 0.006836, 0.007812, 0.009766, 0.011719, 0.013672, 0.015625, 0.019531, 0.023438, 0.027344,
    0.03125, 0.039062, 0.046875, 0.054688, 0.0625, 0.078125, 0.09375, 0.109375, 0.125, 0.15625, 0.1875,
    0.21875, 0.249992, 0.312492, 0.374992, 0.437492, 0.5, 0.625008, 0.750008, 0.875008, 1.0, 1.250015,
    1.500015, 1.750015, 2.0, 2.500031, 3.000031, 3.500031, 4.0, 4.0, 4.0, 4.0,
];

/// Samples an attack takes at each effective rate; `None` never arrives.
pub static ATTACK_SAMPLES: [Option<i64>; 64] = [
    None, None, None, None, Some(141316), Some(112644), Some(94212), Some(79876), Some(70660), Some(56324),
    Some(47108), Some(39940), Some(35332), Some(28164), Some(23556), Some(19972), Some(17668), Some(14084),
    Some(11780), Some(9988), Some(8836), Some(7044), Some(5892), Some(4996), Some(4420), Some(3524),
    Some(2948), Some(2500), Some(2212), Some(1764), Some(1476), Some(1252), Some(1108), Some(884), Some(740),
    Some(628), Some(556), Some(444), Some(372), Some(316), Some(280), Some(224), Some(188), Some(160),
    Some(142), Some(114), Some(96), Some(82), Some(70), Some(54), Some(46), Some(39), Some(35), Some(27),
    Some(22), Some(21), Some(19), Some(12), Some(11), Some(9), Some(0), Some(0), Some(0), Some(0),
];

static GENMIDI: &[u8] = include_bytes!("../data/genmidi.lmp");

fn logsin() -> &'static [i64; 256] {
    static T: OnceLock<[i64; 256]> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = [0i64; 256];
        for (i, v) in t.iter_mut().enumerate() {
            *v = np::round_i64(-(((i as f64 + 0.5) * std::f64::consts::PI / 512.0).sin().log2()) * 256.0);
        }
        t
    })
}

fn exp_table() -> &'static [i64; 256] {
    static T: OnceLock<[i64; 256]> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = [0i64; 256];
        for (i, v) in t.iter_mut().enumerate() {
            *v = np::round_i64(2.0f64.powf((255 - i as i64) as f64 / 256.0) * 1024.0);
        }
        t
    })
}

#[derive(Clone, Copy, Debug)]
pub struct Operator {
    pub am: bool,
    pub vib: bool,
    pub sustained: bool,
    pub ksr: bool,
    pub mult: usize,
    pub ksl: usize,
    pub tl: i64,
    pub ar: i64,
    pub dr: i64,
    pub sl: i64,
    pub rr: i64,
    pub wave: i32,
}

impl Operator {
    fn from_bytes(avek: u8, ad: u8, sr_: u8, wave: u8, ksl: u8, tl: u8) -> Self {
        Operator {
            am: avek & 0x80 != 0,
            vib: avek & 0x40 != 0,
            sustained: avek & 0x20 != 0,
            ksr: avek & 0x10 != 0,
            mult: (avek & 0x0F) as usize,
            ksl: ((ksl >> 6) & 3) as usize,
            tl: (tl & 0x3F) as i64,
            ar: (ad >> 4) as i64,
            dr: (ad & 0x0F) as i64,
            sl: (sr_ >> 4) as i64,
            rr: (sr_ & 0x0F) as i64,
            wave: (wave & 7) as i32,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Voice {
    pub modulator: Operator,
    pub carrier: Operator,
    pub feedback: i64,
    pub additive: bool,
    pub note_offset: i64,
}

#[derive(Clone, Debug)]
pub struct Instrument {
    pub name: String,
    pub voices: Vec<Voice>,
    pub fixed_note: Option<i64>,
    pub fine_tune: i64,
}

/// The 175 GENMIDI instruments: GM programs 0-127, then drum keys 35-81.
pub fn bank() -> &'static [Instrument] {
    static BANK: OnceLock<Vec<Instrument>> = OnceLock::new();
    BANK.get_or_init(|| parse_genmidi(GENMIDI).expect("embedded GENMIDI lump is valid"))
}

pub fn parse_genmidi(data: &[u8]) -> Result<Vec<Instrument>, String> {
    if data.len() < 8 + 175 * 68 || &data[..8] != b"#OPL_II#" {
        return Err("not a GENMIDI lump".into());
    }
    let mut out = Vec::with_capacity(175);
    for i in 0..175 {
        let raw = &data[8 + i * 36..8 + (i + 1) * 36];
        let flags = u16::from_le_bytes([raw[0], raw[1]]);
        let fine = raw[2] as i64;
        let fixed = raw[3] as i64;
        let mut voices = Vec::new();
        for v in 0..(if flags & 0x0004 != 0 { 2 } else { 1 }) {
            let f = &raw[4 + v * 16..4 + (v + 1) * 16];
            let offset = i16::from_le_bytes([f[14], f[15]]) as i64;
            voices.push(Voice {
                modulator: Operator::from_bytes(f[0], f[1], f[2], f[3], f[4], f[5]),
                carrier: Operator::from_bytes(f[7], f[8], f[9], f[10], f[11], f[12]),
                feedback: ((f[6] >> 1) & 7) as i64,
                additive: f[6] & 1 != 0,
                note_offset: offset,
            });
        }
        let base = 8 + 175 * 36 + i * 32;
        let name_raw = &data[base..base + 32];
        let end = name_raw.iter().position(|b| *b == 0).unwrap_or(32);
        out.push(Instrument {
            name: String::from_utf8_lossy(&name_raw[..end]).into_owned(),
            voices,
            fixed_note: if flags & 0x0001 != 0 { Some(fixed) } else { None },
            fine_tune: fine,
        });
    }
    Ok(out)
}

pub fn instrument_for(program: Option<u8>, drum_key: Option<u8>) -> &'static Instrument {
    let b = bank();
    if let Some(k) = drum_key {
        return &b[128 + (k as i64).clamp(35, 81) as usize - 35];
    }
    &b[(program.unwrap_or(0) as usize).min(127)]
}

#[inline]
fn wave_sample(phase: i64, env: f64, kind: i32) -> i64 {
    let ls = logsin();
    let p = phase & 0x3FF;
    let lo = (p & 0xFF) as usize;
    let mirrored = if p & 0x100 != 0 { ls[lo ^ 0xFF] } else { ls[lo] };
    let doubled = if p & 0x80 != 0 {
        ls[(((p ^ 0xFF) << 1) & 0xFF) as usize]
    } else {
        ls[((p << 1) & 0xFF) as usize]
    };
    let mut neg = false;
    let out = match kind {
        0 => {
            neg = p & 0x200 != 0;
            mirrored
        }
        1 => {
            if p & 0x200 != 0 {
                0x1000
            } else {
                mirrored
            }
        }
        2 => mirrored,
        3 => {
            if p & 0x100 != 0 {
                0x1000
            } else {
                ls[lo]
            }
        }
        4 => {
            neg = (p & 0x300) == 0x100;
            if p & 0x200 != 0 {
                0x1000
            } else {
                doubled
            }
        }
        5 => {
            if p & 0x200 != 0 {
                0x1000
            } else {
                doubled
            }
        }
        6 => {
            neg = p & 0x200 != 0;
            0
        }
        _ => {
            neg = p & 0x200 != 0;
            (if neg { (p & 0x1FF) ^ 0x1FF } else { p }) << 3
        }
    };
    let level = out + ((env as i64) << 3);
    let amp = (exp_table()[(level & 0xFF) as usize] << 1) >> (level >> 8).min(31);
    if neg {
        -amp - 1
    } else {
        amp
    }
}

/// The F-number and block the chip would be set to: most precise that fits.
pub fn fnum_block(hz: f64) -> (i64, i64) {
    for block in 0..8 {
        let f = np::round_i64(hz * 1048576.0 / (RATE * 2.0f64.powi(block)));
        if f < 1024 {
            return (f.max(1), block as i64);
        }
    }
    (1023, 7)
}

fn envelope(op: &Operator, n: usize, off: usize, ksv: i64, ksl_units: i64, extra_tl: f64) -> Vec<f64> {
    let ks = ksv >> (if op.ksr { 0 } else { 2 });
    let rate = |reg: i64| -> usize {
        if reg == 0 {
            0
        } else {
            (ks + (reg << 2)).min(63) as usize
        }
    };
    let sl = ((if op.sl == 15 { 0x1F } else { op.sl }) << 4) as f64;
    let atk = if op.ar != 0 { ATTACK_SAMPLES[rate(op.ar)] } else { None };
    let (mut env, t_on): (Vec<f64>, f64) = match atk {
        None => (vec![511.0; n], f64::INFINITY),
        Some(0) => (vec![0.0; n], 0.0),
        Some(a) => {
            let f = 1.0 - 512.0f64.powf(-1.0 / a as f64);
            let base = 1.0 - f;
            (
                (0..n).map(|i| (512.0 * base.powf(i as f64) - 1.0).max(0.0)).collect(),
                a as f64,
            )
        }
    };
    if t_on.is_finite() {
        let d = DECAY_PER_SAMPLE[rate(op.dr)];
        let rr = DECAY_PER_SAMPLE[rate(op.rr)];
        for (i, e) in env.iter_mut().enumerate() {
            let t = i as f64;
            if t < t_on {
                continue;
            }
            let since = (t - t_on).max(0.0);
            let mut fall = sl.min(d * since);
            if !op.sustained && d > 0.0 {
                let later = (since - sl / d).max(0.0);
                fall += rr * later;
            }
            *e = fall.min(511.0);
        }
    }
    if off < n {
        let at_off = env[off.min(n - 1)];
        let rr = DECAY_PER_SAMPLE[rate(op.rr)];
        for (i, e) in env.iter_mut().enumerate() {
            let t = i as f64;
            if t >= off as f64 {
                *e = (at_off + rr * (t - off as f64)).min(511.0);
            }
        }
    }
    let add = (op.tl << 2) as f64 + (ksl_units >> KSL_SHIFT[op.ksl]) as f64;
    env.iter().map(|e| (e + add + extra_tl).min(511.0)).collect()
}

fn tremolo(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| {
            let pos = (i / 64) % 210;
            (if pos < 105 { pos } else { 210 - pos } >> 4) as f64
        })
        .collect()
}

fn render_voice(voice: &Voice, note: f64, n: usize, off: usize, level_units: f64, opl3: bool) -> Vec<f64> {
    let hz = 440.0 * 2.0f64.powf((note - 69.0) / 12.0);
    let (fnum, block) = fnum_block(hz);
    let ksv = (block << 1) | ((fnum >> 9) & 1);
    let ksl_units = ((KSL_ROM[(fnum >> 6) as usize] << 2) - ((8 - block) << 5)).max(0);
    let rng = ((fnum >> 7) & 7) >> 1;
    let vib: Vec<i64> = (0..n)
        .map(|i| {
            let vp = ((i / 1024) & 7) as i64;
            let v = if vp % 4 == 0 {
                0
            } else if vp % 2 == 1 {
                rng >> 1
            } else if vp % 4 == 2 {
                rng
            } else {
                0
            };
            v * if vp >= 4 { -1 } else { 1 }
        })
        .collect();

    let phase = |op: &Operator| -> Vec<i64> {
        let mut acc = 0i64;
        (0..n)
            .map(|i| {
                let f = fnum + if op.vib { vib[i] } else { 0 };
                acc += (((f << block) >> 1) * MULT[op.mult]) >> 1;
                acc >> 9
            })
            .collect()
    };
    let wave = |w: i32| if opl3 { w } else { w & 3 };
    let trem = tremolo(n);
    let (m, c) = (&voice.modulator, &voice.carrier);
    let mut m_env = envelope(m, n, off, ksv, ksl_units, if voice.additive { level_units } else { 0.0 });
    let mut c_env = envelope(c, n, off, ksv, ksl_units, level_units);
    if m.am {
        for (e, t) in m_env.iter_mut().zip(trem.iter()) {
            *e = (*e + t).min(511.0);
        }
    }
    if c.am {
        for (e, t) in c_env.iter_mut().zip(trem.iter()) {
            *e = (*e + t).min(511.0);
        }
    }

    let pm = phase(m);
    let mw = wave(m.wave);
    let mut modl: Vec<i64> = pm.iter().zip(m_env.iter()).map(|(p, e)| wave_sample(*p, *e, mw)).collect();
    if voice.feedback != 0 {
        let shift = 9 - voice.feedback;
        for _ in 0..6 {
            let next: Vec<i64> = (0..n)
                .map(|i| {
                    let prev = if i >= 1 { modl[i - 1] } else { 0 };
                    let prev2 = if i >= 2 { modl[i - 2] } else { 0 };
                    wave_sample(pm[i] + ((prev + prev2) >> shift), m_env[i], mw)
                })
                .collect();
            modl = next;
        }
    }
    let pc = phase(c);
    let cw = wave(c.wave);
    (0..n)
        .map(|i| {
            if voice.additive {
                (modl[i] + wave_sample(pc[i], c_env[i], cw)) as f64
            } else {
                wave_sample(pc[i] + modl[i], c_env[i], cw) as f64
            }
        })
        .collect()
}

/// The OPL2's DAC: a ten-bit mantissa and a three-bit exponent.
pub fn ym3014(y: &[f32]) -> Vec<f32> {
    let full = f32c(32767.0);
    y.iter()
        .map(|v| {
            let s = (v.clamp(-1.0, 1.0) * full).round_ties_even();
            let mag = s.abs();
            let ilog = (mag.max(1.0) as u32).ilog2() as f32;
            let e = (ilog - 8.0).clamp(0.0, 7.0);
            let step = 2.0f32.powf(e);
            (s / step).round_ties_even() * step / full
        })
        .collect()
}

fn highpass(y: &[f64], hz: f64, sr: f64) -> Vec<f64> {
    if y.len() < 32 {
        return y.to_vec();
    }
    let n = util::pow2_ceil(y.len() + 8192);
    let f = np::rfftfreq(n, 1.0 / sr);
    let mut spec = np::rfft(y, n);
    let one = C64::new(1.0, 0.0);
    for (s, fr) in spec.iter_mut().zip(f.iter()) {
        let a = C64::new(0.0, fr * (1.0 / hz));
        *s = cdiv(*s * a, one + a);
    }
    let mut out = np::irfft(&spec, n);
    out.truncate(y.len());
    out
}

/// *instrument* playing *midi_note* held for *dur* seconds, as the chip would.
pub fn render_note(
    instrument: &Instrument,
    midi_note: f64,
    dur: f64,
    sr: u32,
    level: f64,
    opl3: bool,
    tail: f64,
) -> Vec<f32> {
    let n_key = (util::py_round(dur * RATE).max(2)) as usize;
    let n = n_key + (tail * RATE) as usize;
    let level_units = (-20.0 * level.max(1e-3).log10() / 0.1875).min(511.0);
    let mut out = vec![0.0f64; n];
    for (i, voice) in instrument.voices.iter().enumerate() {
        let mut note = match instrument.fixed_note {
            Some(f) => f as f64,
            None => {
                let mut note = midi_note + 12.0 + voice.note_offset as f64;
                while note < 12.0 {
                    note += 12.0;
                }
                while note > 107.0 {
                    note -= 12.0;
                }
                note
            }
        };
        if i == 1 {
            note += ((instrument.fine_tune as f64 / 2.0) - 64.0) / 32.0;
        }
        for (o, s) in out.iter_mut().zip(render_voice(voice, note, n, n_key, level_units, opl3)) {
            *o += s;
        }
    }
    let nv = instrument.voices.len() as f64;
    let y: Vec<f64> = out.iter().map(|v| v / 4084.0 / nv).collect();
    let y = highpass(&y, 20.0, RATE);
    let y = util::lowpass8_f64(&y, 0.45 * sr as f64, RATE, FftLen::Plus(4096));
    let m = (util::py_round(n as f64 * sr as f64 / RATE).max(2)) as usize;
    let pos: Vec<f64> = (0..m).map(|i| i as f64 * RATE / sr as f64).collect();
    let mut y: Vec<f32> = np::interp_arange(&pos, &y).iter().map(|v| *v as f32).collect();
    if !opl3 {
        y = ym3014(&y);
    }
    let edge = m.min((0.003 * sr as f64) as usize);
    util::fade_tail_f32(&mut y, edge);
    let g = f32c(0.9);
    y.iter().map(|v| *v * g).collect()
}

/// The Sound Blaster's DAC, where a game's speech went.
pub fn pcm(y: &[f32], sr: u32, sixteen: bool) -> Vec<f32> {
    if y.is_empty() {
        return Vec::new();
    }
    let rate = if sixteen { 22050.0 } else { 11025.0 };
    let step = (util::py_round(sr as f64 / rate).max(1)) as usize;
    let held = util::hold_samples(y, step);
    let bits = f32c(if sixteen { 32768.0 } else { 128.0 });
    held.iter().map(|h| (*h * bits).round_ties_even() / bits).collect()
}
