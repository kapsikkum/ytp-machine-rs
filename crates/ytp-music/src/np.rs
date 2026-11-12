//! The handful of numpy behaviours the Python synths lean on, reproduced so
//! the Rust port rounds the same way: pairwise summation, `interp`,
//! `linspace`, FFT helpers with numpy's normalisation, and numpy's default
//! random generator (SeedSequence + PCG64 + ziggurat normal).

use std::cell::RefCell;
use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

use crate::ziggurat::{FI_DOUBLE, KI_DOUBLE, WI_DOUBLE, ZIGGURAT_NOR_INV_R, ZIGGURAT_NOR_R};

pub type C64 = Complex<f64>;

#[inline]
pub fn round_even(x: f64) -> f64 {
    x.round_ties_even()
}

/// Python's `round(x)` / `np.round(x)` as an integer.
#[inline]
pub fn round_i64(x: f64) -> i64 {
    x.round_ties_even() as i64
}

#[inline]
pub fn clip(x: f64, lo: f64, hi: f64) -> f64 {
    x.max(lo).min(hi)
}

// ── Summation ────────────────────────────────────────────────────────────────

macro_rules! pairwise {
    ($name:ident, $t:ty) => {
        /// numpy's pairwise sum (`loops_utils.h`): eight accumulators for
        /// blocks up to 128, halving above that.
        pub fn $name(a: &[$t]) -> $t {
            let n = a.len();
            if n < 8 {
                let mut res: $t = 0.0;
                for v in a {
                    res += *v;
                }
                res
            } else if n <= 128 {
                let mut r = [a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]];
                let mut i = 8;
                while i < n - (n % 8) {
                    for j in 0..8 {
                        r[j] += a[i + j];
                    }
                    i += 8;
                }
                let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
                while i < n {
                    res += a[i];
                    i += 1;
                }
                res
            } else {
                let mut n2 = n / 2;
                n2 -= n2 % 8;
                $name(&a[..n2]) + $name(&a[n2..])
            }
        }
    };
}
pairwise!(pairwise_sum_f64, f64);
pairwise!(pairwise_sum_f32, f32);

/// `a.sum()` for a contiguous float64 array (numpy reduces the whole array
/// pairwise, starting from the additive identity).
pub fn sum_f64(a: &[f64]) -> f64 {
    pairwise_sum_f64(a)
}

pub fn sum_f32(a: &[f32]) -> f32 {
    pairwise_sum_f32(a)
}

pub fn mean_f64(a: &[f64]) -> f64 {
    sum_f64(a) / a.len() as f64
}

pub fn mean_f32(a: &[f32]) -> f32 {
    sum_f32(a) / a.len() as f32
}

pub fn std_f64(a: &[f64]) -> f64 {
    let m = mean_f64(a);
    let sq: Vec<f64> = a
        .iter()
        .map(|v| {
            let d = *v - m;
            d * d
        })
        .collect();
    (sum_f64(&sq) / a.len() as f64).sqrt()
}

pub fn median_f64(a: &[f64]) -> f64 {
    let mut v = a.to_vec();
    v.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n == 0 {
        return f64::NAN;
    }
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

pub fn abs_max_f32(a: &[f32]) -> f32 {
    a.iter().fold(0.0f32, |m, v| m.max(v.abs()))
}

pub fn abs_max_f64(a: &[f64]) -> f64 {
    a.iter().fold(0.0f64, |m, v| m.max(v.abs()))
}

// ── Interpolation and ramps ──────────────────────────────────────────────────

/// `np.interp(pos, np.arange(len(y)), y)` for one position.
#[inline]
pub fn interp_arange_at(pos: f64, y: &[f64]) -> f64 {
    let n = y.len();
    if pos.is_nan() {
        return pos;
    }
    if pos < 0.0 {
        return y[0];
    }
    if pos > (n - 1) as f64 {
        return y[n - 1];
    }
    let j = pos.floor() as usize;
    if j >= n - 1 {
        return y[n - 1];
    }
    let xj = j as f64;
    if xj == pos {
        return y[j];
    }
    let slope = (y[j + 1] - y[j]) / ((j + 1) as f64 - xj);
    slope * (pos - xj) + y[j]
}

pub fn interp_arange(pos: &[f64], y: &[f64]) -> Vec<f64> {
    pos.iter().map(|p| interp_arange_at(*p, y)).collect()
}

/// General `np.interp(x, xp, fp)` with the default left/right values.
pub fn interp(x: &[f64], xp: &[f64], fp: &[f64]) -> Vec<f64> {
    let n = xp.len();
    x.iter()
        .map(|&xv| {
            if xv.is_nan() {
                return xv;
            }
            if xv < xp[0] {
                return fp[0];
            }
            if xv > xp[n - 1] {
                return fp[n - 1];
            }
            // largest j with xp[j] <= xv
            let j = match xp.partition_point(|v| *v <= xv) {
                0 => 0,
                k => k - 1,
            };
            if j == n - 1 || xp[j] == xv {
                return fp[j];
            }
            let slope = (fp[j + 1] - fp[j]) / (xp[j + 1] - xp[j]);
            let r = slope * (xv - xp[j]) + fp[j];
            if r.is_nan() {
                slope * (xv - xp[j + 1]) + fp[j + 1]
            } else {
                r
            }
        })
        .collect()
}

/// `np.linspace(start, stop, num)` in float64.
pub fn linspace(start: f64, stop: f64, num: usize) -> Vec<f64> {
    if num == 0 {
        return Vec::new();
    }
    if num == 1 {
        return vec![start];
    }
    let div = (num - 1) as f64;
    let delta = stop - start;
    let step = delta / div;
    let mut y: Vec<f64> = (0..num)
        .map(|i| {
            let v = i as f64;
            if step == 0.0 {
                v / div * delta + start
            } else {
                v * step + start
            }
        })
        .collect();
    y[num - 1] = stop;
    y
}

pub fn linspace_f32(start: f64, stop: f64, num: usize) -> Vec<f32> {
    linspace(start, stop, num).into_iter().map(|v| v as f32).collect()
}

/// `np.hanning(m)`.
pub fn hanning(m: usize) -> Vec<f64> {
    if m < 1 {
        return Vec::new();
    }
    if m == 1 {
        return vec![1.0];
    }
    let mm = m as f64;
    (0..m)
        .map(|i| {
            let n = (1 - m as i64 + 2 * i as i64) as f64;
            0.5 + 0.5 * (std::f64::consts::PI * n / (mm - 1.0)).cos()
        })
        .collect()
}

pub fn next_pow2(n: usize) -> usize {
    n.max(1).next_power_of_two()
}

// ── FFT (numpy conventions) ──────────────────────────────────────────────────

thread_local! {
    static PLANNER: RefCell<RealFftPlanner<f64>> = RefCell::new(RealFftPlanner::<f64>::new());
}

fn plans(n: usize) -> (Arc<dyn RealToComplex<f64>>, Arc<dyn ComplexToReal<f64>>) {
    PLANNER.with(|p| {
        let mut p = p.borrow_mut();
        (p.plan_fft_forward(n), p.plan_fft_inverse(n))
    })
}

/// `np.fft.rfft(x, n)`: zero-pads or truncates *x* to *n* first.
pub fn rfft(x: &[f64], n: usize) -> Vec<C64> {
    let (fwd, _) = plans(n);
    let mut input = vec![0.0f64; n];
    let k = x.len().min(n);
    input[..k].copy_from_slice(&x[..k]);
    let mut out = fwd.make_output_vec();
    fwd.process(&mut input, &mut out).expect("rfft");
    out
}

/// `np.fft.irfft(spec, n)`.
pub fn irfft(spec: &[C64], n: usize) -> Vec<f64> {
    let (_, inv) = plans(n);
    let m = n / 2 + 1;
    let mut input = vec![C64::new(0.0, 0.0); m];
    let k = spec.len().min(m);
    input[..k].copy_from_slice(&spec[..k]);
    input[0].im = 0.0;
    if n.is_multiple_of(2) {
        input[m - 1].im = 0.0;
    }
    let mut out = vec![0.0f64; n];
    inv.process(&mut input, &mut out).expect("irfft");
    let scale = 1.0 / n as f64;
    for v in &mut out {
        *v *= scale;
    }
    out
}

/// `np.fft.rfftfreq(n, 1/sr)` — numpy computes `arange(N) * (1 / (n * d))`.
pub fn rfftfreq(n: usize, d: f64) -> Vec<f64> {
    let val = 1.0 / (n as f64 * d);
    (0..n / 2 + 1).map(|k| k as f64 * val).collect()
}

/// `irfft(rfft(y, n) * h(f), n)[..len]`, the shape of nearly every filter in
/// the Python synths. `h` gets each bin's frequency in Hz for sample rate `sr`.
pub fn fft_filter(y: &[f64], n: usize, sr: f64, h: impl Fn(f64) -> C64) -> Vec<f64> {
    let mut spec = rfft(y, n);
    let f = rfftfreq(n, 1.0 / sr);
    for (s, fr) in spec.iter_mut().zip(f.iter()) {
        *s *= h(*fr);
    }
    let mut out = irfft(&spec, n);
    out.truncate(y.len());
    out
}

// ── numpy.random.default_rng ─────────────────────────────────────────────────

const INIT_A: u32 = 0x43b0_d7e5;
const MULT_A: u32 = 0x931e_8875;
const INIT_B: u32 = 0x8b51_f9dd;
const MULT_B: u32 = 0x58f3_8ded;
const MIX_MULT_L: u32 = 0xca01_f9dd;
const MIX_MULT_R: u32 = 0x4973_f715;
const XSHIFT: u32 = 16;

fn hashmix(value: u32, hash_const: &mut u32) -> u32 {
    let mut v = value ^ *hash_const;
    *hash_const = hash_const.wrapping_mul(MULT_A);
    v = v.wrapping_mul(*hash_const);
    v ^ (v >> XSHIFT)
}

fn mix(x: u32, y: u32) -> u32 {
    let r = MIX_MULT_L.wrapping_mul(x).wrapping_sub(MIX_MULT_R.wrapping_mul(y));
    r ^ (r >> XSHIFT)
}

/// `np.random.SeedSequence(seed).generate_state(4, np.uint64)`.
fn seed_sequence_state(seed: u64) -> [u64; 4] {
    let mut entropy = vec![seed as u32];
    if seed >> 32 != 0 {
        entropy.push((seed >> 32) as u32);
    }
    let mut pool = [0u32; 4];
    let mut hash_const = INIT_A;
    for (i, p) in pool.iter_mut().enumerate() {
        *p = hashmix(entropy.get(i).copied().unwrap_or(0), &mut hash_const);
    }
    for i_src in 0..4 {
        for i_dst in 0..4 {
            if i_src != i_dst {
                let h = hashmix(pool[i_src], &mut hash_const);
                pool[i_dst] = mix(pool[i_dst], h);
            }
        }
    }
    let mut hash_const = INIT_B;
    let mut words = [0u32; 8];
    for (i, w) in words.iter_mut().enumerate() {
        let mut d = pool[i % 4] ^ hash_const;
        hash_const = hash_const.wrapping_mul(MULT_B);
        d = d.wrapping_mul(hash_const);
        *w = d ^ (d >> XSHIFT);
    }
    let mut out = [0u64; 4];
    for i in 0..4 {
        out[i] = words[2 * i] as u64 | ((words[2 * i + 1] as u64) << 32);
    }
    out
}

const PCG_MULT: u128 = 0x2360_ED05_1FC6_5DA4_4385_DF64_9FCC_F645;

/// numpy's `default_rng(seed)`: PCG64 (XSL-RR 128/64) seeded via SeedSequence.
#[derive(Clone)]
pub struct Rng {
    state: u128,
    inc: u128,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        let v = seed_sequence_state(seed);
        let initstate = ((v[0] as u128) << 64) | v[1] as u128;
        let initseq = ((v[2] as u128) << 64) | v[3] as u128;
        let mut r = Rng { state: 0, inc: (initseq << 1) | 1 };
        r.step();
        r.state = r.state.wrapping_add(initstate);
        r.step();
        r
    }

    #[inline]
    fn step(&mut self) {
        self.state = self.state.wrapping_mul(PCG_MULT).wrapping_add(self.inc);
    }

    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.step();
        let s = self.state;
        let x = ((s >> 64) as u64) ^ (s as u64);
        x.rotate_right((s >> 122) as u32)
    }

    #[inline]
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0)
    }

    /// `rng.standard_normal()` (ziggurat).
    pub fn standard_normal(&mut self) -> f64 {
        loop {
            let mut r = self.next_u64();
            let idx = (r & 0xff) as usize;
            r >>= 8;
            let sign = r & 1;
            let rabs = (r >> 1) & 0x000f_ffff_ffff_ffff;
            let mut x = rabs as f64 * WI_DOUBLE[idx];
            if sign == 1 {
                x = -x;
            }
            if rabs < KI_DOUBLE[idx] {
                return x;
            }
            if idx == 0 {
                loop {
                    let xx = -ZIGGURAT_NOR_INV_R * (-self.next_f64()).ln_1p();
                    let yy = -(-self.next_f64()).ln_1p();
                    if yy + yy > xx * xx {
                        return if (rabs >> 8) & 1 == 1 {
                            -(ZIGGURAT_NOR_R + xx)
                        } else {
                            ZIGGURAT_NOR_R + xx
                        };
                    }
                }
            } else if (FI_DOUBLE[idx - 1] - FI_DOUBLE[idx]) * self.next_f64() + FI_DOUBLE[idx]
                < (-0.5 * x * x).exp()
            {
                return x;
            }
        }
    }

    pub fn standard_normal_vec(&mut self, n: usize) -> Vec<f64> {
        (0..n).map(|_| self.standard_normal()).collect()
    }
}

/// `np.fft.rfft` of float32 input.
///
/// numpy 2 returns complex64 for float32 input, but the transform itself is
/// evaluated in double precision and only the result is rounded to float32
/// (checked bit-for-bit against numpy 2.4 in `parity_extra.rs`).
pub fn rfft_f32(x: &[f32], n: usize) -> Vec<C64> {
    let xd: Vec<f64> = x.iter().map(|v| *v as f64).collect();
    let mut spec = rfft(&xd, n);
    for s in &mut spec {
        s.re = s.re as f32 as f64;
        s.im = s.im as f32 as f64;
    }
    spec
}
