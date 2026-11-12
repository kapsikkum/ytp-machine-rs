//! The SNES S-DSP (port of `app/ytpmv/snes.py`): BRR-compressed sample
//! playback through the Gaussian interpolator, with the echo unit. The
//! instrument bank (a cut-down General MIDI SoundFont) is embedded.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::np;
use crate::snes_gauss::GAUSS;
use crate::util::{self, f32c};

pub const DSP_RATE: f64 = 32000.0;
pub const BRR_FULL: f64 = 16376.0;
pub const BRR_FILTERS: [(f64, f64); 4] = [(0.0, 0.0), (0.9375, 0.0), (1.90625, -0.9375), (1.796875, -0.8125)];
pub const MAX_STEP: f64 = 0x3FFF as f64 / 0x1000 as f64;
const ECHO_TAP0: f64 = 127.0 / 128.0;

static BANK_BIN: &[u8] = include_bytes!("../data/snes_bank.bin");

#[derive(Clone, Debug)]
pub struct Instrument {
    pub name: String,
    /// float32, -1..1, BRR already done
    pub pcm: Vec<f32>,
    pub rate: f64,
    pub loop_range: Option<(i64, i64)>,
    pub root: i64,
    pub cents: i64,
    /// 0 for a drum that plays one pitch whatever the key
    pub scale: f64,
    pub attenuation_db: f64,
    /// delay, attack, hold, decay, sustain (dB), release
    pub env: [f64; 6],
}

pub struct Bank {
    pub instruments: HashMap<String, Instrument>,
    /// The SoundFont the bank was cut from, as JSON.
    pub source: String,
}

struct Reader<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> &'a [u8] {
        let s = &self.b[self.p..self.p + n];
        self.p += n;
        s
    }
    fn u8(&mut self) -> u8 {
        self.take(1)[0]
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take(4).try_into().unwrap())
    }
    fn i32(&mut self) -> i32 {
        i32::from_le_bytes(self.take(4).try_into().unwrap())
    }
    fn f64(&mut self) -> f64 {
        f64::from_le_bytes(self.take(8).try_into().unwrap())
    }
    fn str(&mut self) -> String {
        let n = self.u8() as usize;
        String::from_utf8_lossy(self.take(n)).into_owned()
    }
}

/// Parse the `SNB1` bank format written by `tools/convert_snes_bank.py`.
pub fn parse_bank(data: &[u8]) -> Result<Bank, String> {
    if data.len() < 16 || &data[..4] != b"SNB1" {
        return Err("not an SNB1 bank".into());
    }
    let mut r = Reader { b: data, p: 4 };
    let count = r.u32() as usize;
    let pcm_len = r.u32() as usize;
    let src_len = r.u32() as usize;
    let source = String::from_utf8_lossy(r.take(src_len)).into_owned();
    struct Raw {
        key: String,
        name: String,
        rate: f64,
        lp: (i32, i32),
        root: i32,
        cents: i32,
        scale: f64,
        att: f64,
        env: [f64; 6],
        offset: usize,
        length: usize,
    }
    let mut raws = Vec::with_capacity(count);
    for _ in 0..count {
        let key = r.str();
        let name = r.str();
        let rate = r.f64();
        let lp = (r.i32(), r.i32());
        let root = r.i32();
        let cents = r.i32();
        let scale = r.f64();
        let att = r.f64();
        let mut env = [0.0; 6];
        for e in &mut env {
            *e = r.f64();
        }
        let offset = r.u32() as usize;
        let length = r.u32() as usize;
        raws.push(Raw { key, name, rate, lp, root, cents, scale, att, env, offset, length });
    }
    let pcm_bytes = r.take(pcm_len * 2);
    let pcm: Vec<f32> = pcm_bytes
        .as_chunks::<2>().0.iter()
        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / f32c(32767.0))
        .collect();
    let mut instruments = HashMap::new();
    for raw in raws {
        let slice = pcm.get(raw.offset..raw.offset + raw.length).ok_or("sample out of range")?;
        instruments.insert(
            raw.key,
            Instrument {
                name: raw.name,
                pcm: slice.to_vec(),
                rate: raw.rate,
                loop_range: if raw.lp.0 >= 0 { Some((raw.lp.0 as i64, raw.lp.1 as i64)) } else { None },
                root: raw.root as i64,
                cents: raw.cents as i64,
                scale: raw.scale,
                attenuation_db: raw.att,
                env: raw.env,
            },
        );
    }
    Ok(Bank { instruments, source })
}

pub fn bank() -> &'static Bank {
    static BANK: OnceLock<Bank> = OnceLock::new();
    BANK.get_or_init(|| parse_bank(BANK_BIN).expect("embedded SNES bank is valid"))
}

pub fn instrument_for(program: Option<u8>, drum_key: Option<u8>) -> &'static Instrument {
    let b = &bank().instruments;
    if let Some(k) = drum_key {
        return b.get(&format!("d{k}")).unwrap_or_else(|| &b["d38"]);
    }
    b.get(&format!("p{}", program.unwrap_or(0))).unwrap_or_else(|| &b["p0"])
}

pub fn resample(y: &[f32], src: f64, dst: f64) -> Vec<f32> {
    if (src - dst).abs() < 1e-6 || y.len() < 2 {
        return y.to_vec();
    }
    let n = (util::py_round(y.len() as f64 * dst / src).max(2)) as usize;
    let yd: Vec<f64> = y.iter().map(|v| *v as f64).collect();
    let pos: Vec<f64> = (0..n).map(|i| i as f64 * src / dst).collect();
    np::interp_arange(&pos, &yd).iter().map(|v| *v as f32).collect()
}

/// *y* squeezed into four bits a sample and unpacked again.
pub fn brr(y: &[f32]) -> Vec<f32> {
    let n = y.len();
    if n < 32 {
        return y.to_vec();
    }
    let peak = np::abs_max_f32(y) as f64;
    if peak <= 0.0 {
        return y.to_vec();
    }
    let scale = BRR_FULL / peak;
    let yd: Vec<f64> = y.iter().map(|v| *v as f64 * scale).collect();
    let blocks = n / 16;
    let t = |b: usize, i: usize| yd[b * 16 + i];
    let prev1: Vec<f64> = (0..blocks).map(|b| if b == 0 { 0.0 } else { yd[b * 16 - 1] }).collect();
    // the Python slices y[14::16] after two zeros, so block 1 also gets 0.0
    let prev2: Vec<f64> = (0..blocks).map(|b| if b < 2 { 0.0 } else { yd[b * 16 - 2] }).collect();
    let mut best = vec![0usize; blocks];
    let mut best_err = vec![0.0f64; blocks];
    for (f, (a, b)) in BRR_FILTERS.iter().enumerate() {
        for blk in 0..blocks {
            let mut err = 0.0f64;
            for i in 0..16 {
                let d1 = if i == 0 { prev1[blk] } else { t(blk, i - 1) };
                let d2 = match i {
                    0 => prev2[blk],
                    1 => prev1[blk],
                    _ => t(blk, i - 2),
                };
                let res = t(blk, i) - a * d1 - b * d2;
                err = err.max(res.abs());
            }
            if f == 0 || err < best_err[blk] {
                if f == 0 {
                    best[blk] = 0;
                } else {
                    best[blk] = f;
                }
                best_err[blk] = err;
            }
        }
    }
    let mut got = Vec::with_capacity(n);
    for blk in 0..blocks {
        let shift = np::clip((best_err[blk].max(1e-9) / 7.0).log2().ceil() + 1.0, 1.0, 12.0);
        let step = 2.0f64.powf(shift) / 2.0;
        let (a, b) = BRR_FILTERS[best[blk]];
        let (mut d1, mut d2) = (prev1[blk], prev2[blk]);
        for i in 0..16 {
            let want = t(blk, i) - a * d1 - b * d2;
            let nib = np::clip(np::round_even(want / step), -8.0, 7.0);
            let d = nib * step + a * d1 + b * d2;
            got.push(d);
            d2 = d1;
            d1 = d;
        }
    }
    got.extend_from_slice(&yd[blocks * 16..]);
    let k = peak / BRR_FULL;
    got.iter().map(|v| (v * k) as f32).collect()
}

/// *y* played back through the chip's four-point interpolation.
pub fn interpolate(y: &[f32], ratio: f64) -> Vec<f32> {
    if y.len() < 8 {
        return y.to_vec();
    }
    let n = ((y.len() as f64 / ratio.max(1e-6)) as usize).max(2);
    (0..n)
        .map(|i| {
            let pos = i as f64 * ratio;
            let idx = (pos as i64).clamp(3, y.len() as i64 - 1) as usize;
            let phase = (((pos - pos.floor()) * 256.0) as i64).clamp(0, 255) as usize;
            let (newest, old, older, oldest) = (y[idx] as f64, y[idx - 1] as f64, y[idx - 2] as f64, y[idx - 3] as f64);
            let out = GAUSS[0xFF - phase] as f64 * oldest
                + GAUSS[0x1FF - phase] as f64 * older
                + GAUSS[0x100 + phase] as f64 * old
                + GAUSS[phase] as f64 * newest;
            (out / 2048.0) as f32
        })
        .collect()
}

/// The delay line, fed back through its filter (the default is a single tap).
pub fn echo(y: &[f32], delay_ms: f64, feedback: f64) -> Vec<f32> {
    let d = ((delay_ms / 1000.0 * DSP_RATE) as usize).max(8);
    if y.len() <= d || feedback <= 0.0 {
        return y.to_vec();
    }
    let mut out: Vec<f64> = y.iter().map(|v| *v as f64).collect();
    let mut start = d;
    while start < out.len() {
        let stop = (start + d).min(out.len());
        for i in 0..(stop - start) {
            let past = out[start - d + i];
            out[start + i] += feedback * (past * ECHO_TAP0);
        }
        start += d;
    }
    out.iter().map(|v| *v as f32).collect()
}

/// A recording as this machine would have played it back.
pub fn play(y: &[f32], sr: u32) -> Vec<f32> {
    play_with(y, sr, 48.0, 0.45, 0.35, 16000.0)
}

pub fn play_with(y: &[f32], sr: u32, delay_ms: f64, feedback: f64, tail: f64, stored_hz: f64) -> Vec<f32> {
    if y.is_empty() {
        return Vec::new();
    }
    let peak = np::abs_max_f32(y) as f64;
    if peak <= 0.0 {
        return y.to_vec();
    }
    let store = np::clip(stored_hz, 2000.0, DSP_RATE);
    let pf = peak as f32;
    let norm: Vec<f32> = y.iter().map(|v| *v / pf).collect();
    let x = resample(&norm, sr as f64, store);
    let x = brr(&x);
    let mut x = interpolate(&x, store / DSP_RATE);
    if tail > 0.0 {
        x.extend(std::iter::repeat_n(0.0f32, (tail * DSP_RATE) as usize));
    }
    let x = echo(&x, delay_ms, feedback);
    let out = resample(&x, DSP_RATE, sr as f64);
    let top = np::abs_max_f32(&out);
    if top > 0.0 {
        out.iter().map(|v| *v / top * pf).collect()
    } else {
        out
    }
}

fn envelope(n: usize, gate: usize, env: &[f64; 6], sr: f64) -> Vec<f64> {
    let [delay, attack, hold, decay, sustain_db, release] = *env;
    let t: Vec<f64> = (0..n).map(|i| i as f64 / sr).collect();
    let (a0, a1) = (delay, delay + attack);
    let h1 = a1 + hold;
    let d1 = if decay > 0.0 { h1 + decay * (sustain_db / 96.0).min(1.0) } else { h1 };
    let mut amp: Vec<f64> = t
        .iter()
        .map(|&tv| {
            let db = if tv < d1 {
                -sustain_db * np::clip((tv - h1) / (d1 - h1).max(1e-9), 0.0, 1.0)
            } else {
                -sustain_db
            };
            if tv < a1 {
                if attack > 0.0 {
                    np::clip((tv - a0) / attack.max(1e-9), 0.0, 1.0)
                } else if tv >= a0 {
                    1.0
                } else {
                    0.0
                }
            } else {
                10.0f64.powf(db / 20.0)
            }
        })
        .collect();
    if gate < n {
        let start = if gate > 0 { amp[gate - 1] } else { 0.0 };
        for i in gate..n {
            let fall = (t[i] - t[gate]) / release.max(0.01) * 96.0;
            amp[i] = start * 10.0f64.powf(-fall / 20.0);
        }
    }
    amp
}

/// One note of *inst*, pitched through the Gaussian table, looped if the
/// sample loops, enveloped, and into the echo.
pub fn render_note(inst: &Instrument, midi_note: f64, dur: f64, sr: u32, level: f64, tail: f64) -> Vec<f32> {
    let semis = (midi_note - inst.root as f64) * inst.scale + inst.cents as f64 / 100.0;
    let step = 2.0f64.powf(semis / 12.0) * inst.rate / DSP_RATE;
    let release = inst.env[5].clamp(0.02, 1.0);
    let gate = ((dur * DSP_RATE) as usize).max(1);
    let mut n = gate + ((release + tail) * DSP_RATE) as usize;
    let y = &inst.pcm;
    let span: i64;
    if let Some((_, le)) = inst.loop_range {
        span = le;
    } else {
        span = y.len() as i64;
        n = n.min(((span - 1).max(0) as f64 / step) as usize);
    }
    if n == 0 {
        return vec![0.0; ((dur * sr as f64) as usize).max(1)];
    }
    let wrap = |k: i64| -> i64 {
        match inst.loop_range {
            Some((ls, le)) if k >= le => {
                ls + (k - ls).rem_euclid(le - ls)
            }
            _ => k,
        }
    };
    let mut out: Vec<f64> = Vec::with_capacity(n);
    for i in 0..n {
        let mut pos = i as f64 * step;
        if let Some((ls, le)) = inst.loop_range {
            if pos >= le as f64 {
                let (lsf, lef) = (ls as f64, le as f64);
                pos = lsf + (pos - lsf) % (lef - lsf);
            }
        }
        let whole = pos.floor() as i64;
        let phase = (((pos - whole as f64) * 256.0) as i64).clamp(0, 255) as usize;
        let at = |k: i64| -> f64 {
            let kk = wrap(whole + k).clamp(0, span - 1);
            y[kk as usize] as f64
        };
        let v = (GAUSS[0xFF - phase] as f64 * at(-2)
            + GAUSS[0x1FF - phase] as f64 * at(-1)
            + GAUSS[0x100 + phase] as f64 * at(0)
            + GAUSS[phase] as f64 * at(1))
            / 2048.0;
        out.push(v);
    }
    let gain = level * 10.0f64.powf(-0.4 * inst.attenuation_db / 20.0);
    let env = envelope(out.len(), gate.min(out.len()), &inst.env, DSP_RATE);
    let shaped: Vec<f32> = out.iter().zip(env.iter()).map(|(o, e)| (o * e * gain) as f32).collect();
    let echoed = echo(&shaped, 48.0, 0.45);
    resample(&echoed, DSP_RATE, sr as f64)
}
