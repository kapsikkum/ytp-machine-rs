//! Python's `random` module (Mersenne Twister, CPython seeding and the
//! `choices` / `choice` / `sample` / `uniform` recipes), so a seed picks the
//! same takes the Python app picked.

use sha2::{Digest, Sha512};

const N: usize = 624;
const M: usize = 397;

pub struct PyRandom {
    mt: [u32; N],
    idx: usize,
}

impl PyRandom {
    fn init_genrand(s: u32) -> [u32; N] {
        let mut mt = [0u32; N];
        mt[0] = s;
        for i in 1..N {
            mt[i] = 1812433253u32
                .wrapping_mul(mt[i - 1] ^ (mt[i - 1] >> 30))
                .wrapping_add(i as u32);
        }
        mt
    }

    fn from_key(key: &[u32]) -> Self {
        let mut mt = Self::init_genrand(19650218);
        let (mut i, mut j) = (1usize, 0usize);
        for _ in 0..N.max(key.len()) {
            mt[i] = (mt[i] ^ (mt[i - 1] ^ (mt[i - 1] >> 30)).wrapping_mul(1664525))
                .wrapping_add(key[j])
                .wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i >= N {
                mt[0] = mt[N - 1];
                i = 1;
            }
            if j >= key.len() {
                j = 0;
            }
        }
        for _ in 0..N - 1 {
            mt[i] = (mt[i] ^ (mt[i - 1] ^ (mt[i - 1] >> 30)).wrapping_mul(1566083941))
                .wrapping_sub(i as u32);
            i += 1;
            if i >= N {
                mt[0] = mt[N - 1];
                i = 1;
            }
        }
        mt[0] = 0x8000_0000;
        Self { mt, idx: N }
    }

    /// `random.seed(n)` for a non-negative int.
    pub fn from_int(n: u64) -> Self {
        let mut key = vec![n as u32];
        if n >> 32 != 0 {
            key.push((n >> 32) as u32);
        }
        Self::from_key(&key)
    }

    /// `random.seed(s)` for a str (version 2: the text plus its SHA-512).
    pub fn from_str_seed(s: &str) -> Self {
        let mut bytes = s.as_bytes().to_vec();
        bytes.extend_from_slice(&Sha512::digest(s.as_bytes()));
        // Big-endian integer -> little-endian 32-bit words, no empty top words.
        let first = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
        let mag = &bytes[first..];
        if mag.is_empty() {
            return Self::from_key(&[0]);
        }
        let mut key = Vec::new();
        let mut end = mag.len();
        while end > 0 {
            let start = end.saturating_sub(4);
            let mut w = 0u32;
            for &b in &mag[start..end] {
                w = (w << 8) | b as u32;
            }
            key.push(w);
            end = start;
        }
        Self::from_key(&key)
    }

    fn genrand(&mut self) -> u32 {
        if self.idx >= N {
            for k in 0..N {
                let y = (self.mt[k] & 0x8000_0000) | (self.mt[(k + 1) % N] & 0x7fff_ffff);
                let mut v = self.mt[(k + M) % N] ^ (y >> 1);
                if y & 1 != 0 {
                    v ^= 0x9908_b0df;
                }
                self.mt[k] = v;
            }
            self.idx = 0;
        }
        let mut y = self.mt[self.idx];
        self.idx += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^= y >> 18;
        y
    }

    /// `random.random()`
    pub fn random(&mut self) -> f64 {
        let a = (self.genrand() >> 5) as f64;
        let b = (self.genrand() >> 6) as f64;
        (a * 67108864.0 + b) * (1.0 / 9007199254740992.0)
    }

    fn getrandbits(&mut self, k: u32) -> u32 {
        self.genrand() >> (32 - k)
    }

    /// `random._randbelow(n)` for 0 < n <= 2^32.
    pub fn randbelow(&mut self, n: u64) -> u64 {
        if n == 0 {
            return 0;
        }
        let k = 64 - n.leading_zeros();
        loop {
            let r = if k <= 32 {
                self.getrandbits(k) as u64
            } else {
                let lo = self.genrand() as u64;
                let hi = (self.genrand() >> (64 - k)) as u64;
                (hi << 32) | lo
            };
            if r < n {
                return r;
            }
        }
    }

    /// `random.uniform(a, b)`
    pub fn uniform(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * self.random()
    }

    /// `random.choice(seq)` -> index.
    pub fn choice_index(&mut self, len: usize) -> usize {
        self.randbelow(len as u64) as usize
    }

    /// `random.choices(population, weights)[0]` -> index.
    pub fn choices_index(&mut self, weights: &[f64]) -> usize {
        let mut cum = Vec::with_capacity(weights.len());
        let mut acc = 0.0;
        for w in weights {
            acc += w;
            cum.push(acc);
        }
        let total = cum[cum.len() - 1] + 0.0;
        let hi = weights.len() - 1;
        let x = self.random() * total;
        // bisect_right(cum, x, 0, hi)
        let (mut lo, mut h) = (0usize, hi);
        while lo < h {
            let mid = (lo + h) / 2;
            if x < cum[mid] {
                h = mid;
            } else {
                lo = mid + 1;
            }
        }
        lo
    }

    /// `random.sample(range(n), k)` -> the chosen indices in order.
    pub fn sample(&mut self, n: usize, k: usize) -> Vec<usize> {
        let mut setsize = 21usize;
        if k > 5 {
            let exp = ((k * 3) as f64).ln() / 4f64.ln();
            setsize += 4usize.pow(exp.ceil() as u32);
        }
        let mut out = Vec::with_capacity(k);
        if n <= setsize {
            let mut pool: Vec<usize> = (0..n).collect();
            for i in 0..k {
                let j = self.randbelow((n - i) as u64) as usize;
                out.push(pool[j]);
                pool[j] = pool[n - i - 1];
            }
        } else {
            let mut selected = std::collections::HashSet::new();
            for _ in 0..k {
                let mut j = self.randbelow(n as u64) as usize;
                while selected.contains(&j) {
                    j = self.randbelow(n as u64) as usize;
                }
                selected.insert(j);
                out.push(j);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference values from CPython 3.13: random.seed(...) then the call.
    #[test]
    fn matches_cpython() {
        assert_eq!(PyRandom::from_int(1).random(), 0.13436424411240122);
        assert_eq!(PyRandom::from_int(0).random(), 0.8444218515250481);
        assert_eq!(PyRandom::from_int(20260913).random(), 0.09548959599513052);
        assert_eq!(PyRandom::from_int(2u64.pow(40) + 5).random(), 0.5043802970418443);
        assert_eq!(PyRandom::from_str_seed("abc").random(), 0.7720246314157545);
        let mut r = PyRandom::from_str_seed("12345");
        assert_eq!(r.random(), 0.45697818440486826);
        assert_eq!(r.random(), 0.7308852116749263);
        let mut r = PyRandom::from_str_seed("hello-world.1");
        assert_eq!(r.choices_index(&[1.0, 2.0, 3.0, 4.0, 5.0]), 4);
        assert_eq!(r.choice_index(7), 1);
        assert_eq!(r.sample(13, 2), vec![8, 10]);
        assert_eq!(r.randbelow(1 << 30), 863100218);
        assert_eq!(r.uniform(1.0, 3.0), 2.8310009781988033);
        let mut r = PyRandom::from_int(20260913);
        r.random();
        assert_eq!(r.randbelow(1 << 30), 407557419);
    }
}
