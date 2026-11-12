//! Put the picture through the console as well as the sound
//! (`app/ytpmv/screen.py`).
//!
//! A frame is cut down to what the machine could actually put on a
//! television: its resolution, and its colours and no others. It comes back at
//! that size and is blown up again during the encode, so what you see is
//! chunky and banded the way the hardware was.
//!
//! * md: 320x224, nine bits of colour; the eight levels are measured off
//!   hardware (0, 52, 87, 116, 144, 172, 206, 255), not a linear ramp
//! * nes: 256x240 and the 2C02's sixty-four fixed colours (FCEUX's palette)
//! * sms: 256x192, two bits a channel
//! * snes: 256x224, five bits a channel
//! * gb: 160x144, four shades of green; gbc: five bits through an LCD that
//!   bled its colours into each other and never reached full brightness
//! * ega / vga / c64: the PC and C64 palettes of their era

use std::sync::OnceLock;

/// `(name, width, height)` of what each machine put on the screen.
pub const SIZES: &[(&str, usize, usize)] = &[
    ("md", 320, 224),
    ("nes", 256, 240),
    ("sms", 256, 192),
    ("snes", 256, 224),
    ("gb", 160, 144),
    ("gbc", 160, 144),
    ("ega", 320, 200),
    ("vga", 320, 200),
    ("c64", 320, 200),
];

pub fn size_of(screen: &str) -> Option<(usize, usize)> {
    SIZES.iter().find(|(n, _, _)| *n == screen).map(|(_, w, h)| (*w, *h))
}

const MD_LEVELS: [u8; 8] = [0, 52, 87, 116, 144, 172, 206, 255];
const SMS_LEVELS: [u8; 4] = [0, 85, 170, 255];

const NES_HEX: [&str; 64] = [
    "747474", "24188c", "0000a8", "44009c", "8c0074", "a80010", "a40000", "7c0800", "402c00", "004400", "005000",
    "003c14", "183c5c", "000000", "000000", "000000", "bcbcbc", "0070ec", "2038ec", "8000f0", "bc00bc", "e40058",
    "d82800", "c84c0c", "887000", "009400", "00a800", "009038", "008088", "000000", "000000", "000000", "fcfcfc",
    "3cbcfc", "5c94fc", "cc88fc", "f478fc", "fc74b4", "fc7460", "fc9838", "f0bc3c", "80d010", "4cdc48", "58f898",
    "00e8d8", "787878", "000000", "000000", "fcfcfc", "a8e4fc", "c4d4fc", "d4c8fc", "fcc4fc", "fcc4d8", "fcbcb0",
    "fcd8a8", "fce4a0", "e0fca0", "a8f0bc", "b0fccc", "9cfcf0", "c4c4c4", "000000", "000000",
];

const EGA_HEX: [&str; 16] = [
    "000000", "0000aa", "00aa00", "00aaaa", "aa0000", "aa00aa", "aa5500", "aaaaaa", "555555", "5555ff", "55ff55",
    "55ffff", "ff5555", "ff55ff", "ffff55", "ffffff",
];

const C64_HEX: [&str; 16] = [
    "000000", "ffffff", "96282e", "5bd6ce", "9f2dad", "41b936", "2724c4", "eff347", "9f4815", "5e3500", "da5f66",
    "474747", "787878", "91ff84", "6864ff", "aeaeae",
];

/// The Game Boy's four shades, lightest first, as ares draws them.
const GB_SHADES: [[u8; 3]; 4] = [[0xAE, 0xD9, 0x27], [0x58, 0xA0, 0x28], [0x20, 0x62, 0x29], [0x1A, 0x45, 0x2A]];

fn hex(h: &str) -> [u8; 3] {
    let v = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0);
    [v(0), v(2), v(4)]
}

fn snes_levels() -> [u8; 32] {
    std::array::from_fn(|v| (((v << 3) | (v >> 2)) & 0xff) as u8)
}

fn vga_levels() -> [u8; 64] {
    std::array::from_fn(|v| (((v << 2) | (v >> 4)) & 0xff) as u8)
}

/// For every byte a channel might be, the level it comes out at.
fn level_lut(levels: &[u8]) -> [u8; 256] {
    std::array::from_fn(|v| {
        let mut best = 0usize;
        for (i, l) in levels.iter().enumerate() {
            if (v as i32 - *l as i32).abs() < (v as i32 - levels[best] as i32).abs() {
                best = i;
            }
        }
        levels[best]
    })
}

/// The nearest of a fixed palette for every colour rounded to five bits,
/// weighted towards green, where the eye keeps most of its judgement.
fn fixed_lut(pal: &[[u8; 3]]) -> Vec<[u8; 3]> {
    let step: Vec<f64> = (0..32).map(|i| i as f64 * 255.0 / 31.0).collect();
    let w = [0.30, 0.59, 0.11];
    let mut out = Vec::with_capacity(32768);
    for r in &step {
        for g in &step {
            for b in &step {
                let mut best = 0usize;
                let mut best_d = f64::INFINITY;
                for (i, p) in pal.iter().enumerate() {
                    let d = (r - p[0] as f64).powi(2) * w[0] + (g - p[1] as f64).powi(2) * w[1] + (b - p[2] as f64).powi(2) * w[2];
                    if d < best_d {
                        best_d = d;
                        best = i;
                    }
                }
                out.push(pal[best]);
            }
        }
    }
    out
}

struct Luts {
    md: [u8; 256],
    sms: [u8; 256],
    snes: [u8; 256],
    vga: [u8; 256],
    nes: Vec<[u8; 3]>,
    ega: Vec<[u8; 3]>,
    c64: Vec<[u8; 3]>,
}

fn luts() -> &'static Luts {
    static L: OnceLock<Luts> = OnceLock::new();
    L.get_or_init(|| {
        let pal = |h: &[&str]| -> Vec<[u8; 3]> { h.iter().map(|s| hex(s)).collect() };
        Luts {
            md: level_lut(&MD_LEVELS),
            sms: level_lut(&SMS_LEVELS),
            snes: level_lut(&snes_levels()),
            vga: level_lut(&vga_levels()),
            nes: fixed_lut(&pal(&NES_HEX)),
            ega: fixed_lut(&pal(&EGA_HEX)),
            c64: fixed_lut(&pal(&C64_HEX)),
        }
    })
}

/// Down to `w`x`h`, averaging what falls in each new pixel (averaged rather
/// than sampled: picking one pixel out of every eight makes the edges crawl).
fn shrink(img: &[u8], sw: usize, sh: usize, w: usize, h: usize) -> Vec<u8> {
    if sw == w && sh == h {
        return img.to_vec();
    }
    let edges = |src: usize, dst: usize| -> Vec<usize> {
        (0..=dst).map(|i| (i as f64 * src as f64 / dst as f64) as usize).collect()
    };
    let (ys, xs) = (edges(sh, h), edges(sw, w));
    let mut out = vec![0u8; w * h * 3];
    for y in 0..h {
        let (y0, y1) = (ys[y], ys[y + 1].max(ys[y] + 1).min(sh));
        for x in 0..w {
            let (x0, x1) = (xs[x], xs[x + 1].max(xs[x] + 1).min(sw));
            let count = ((y1 - y0) * (x1 - x0)).max(1) as u32;
            for c in 0..3 {
                let mut sum = 0u32;
                for yy in y0..y1 {
                    for xx in x0..x1 {
                        sum += img[(yy * sw + xx) * 3 + c] as u32;
                    }
                }
                out[(y * w + x) * 3 + c] = (sum / count) as u8;
            }
        }
    }
    out
}

/// `frame` (RGB24, `sw`x`sh`) reduced to what the named machine could show;
/// it comes back at the machine's own size. An unknown screen passes through.
pub fn apply(frame: &[u8], sw: usize, sh: usize, screen: &str) -> Vec<u8> {
    let Some((w, h)) = size_of(screen) else { return frame.to_vec() };
    let mut px = shrink(frame, sw, sh, w, h);
    let l = luts();
    let per_channel = |px: &mut [u8], lut: &[u8; 256]| {
        for v in px.iter_mut() {
            *v = lut[*v as usize];
        }
    };
    let fixed = |px: &[u8], table: &[[u8; 3]]| -> Vec<u8> {
        let mut out = Vec::with_capacity(px.len());
        for p in px.as_chunks::<3>().0 {
            let idx = ((p[0] >> 3) as usize) * 1024 + ((p[1] >> 3) as usize) * 32 + (p[2] >> 3) as usize;
            out.extend_from_slice(&table[idx]);
        }
        out
    };
    match screen {
        "md" => {
            per_channel(&mut px, &l.md);
            px
        }
        "sms" => {
            per_channel(&mut px, &l.sms);
            px
        }
        "snes" => {
            per_channel(&mut px, &l.snes);
            px
        }
        "vga" => {
            per_channel(&mut px, &l.vga);
            px
        }
        "gb" => {
            let mut out = Vec::with_capacity(px.len());
            for p in px.as_chunks::<3>().0 {
                let lum = (p[0] as i32 * 299 + p[1] as i32 * 587 + p[2] as i32 * 114) / 1000;
                out.extend_from_slice(&GB_SHADES[(3 - (lum / 64).clamp(0, 3)) as usize]);
            }
            out
        }
        "gbc" => {
            let mut out = Vec::with_capacity(px.len());
            for p in px.as_chunks::<3>().0 {
                let (r, g, b) = ((p[0] >> 3) as i32, (p[1] >> 3) as i32, (p[2] >> 3) as i32);
                let rr = (r * 26 + g * 4 + b * 2).min(960);
                let gg = (g * 24 + b * 8).min(960);
                let bb = (r * 6 + g * 4 + b * 22).min(960);
                out.extend_from_slice(&[(rr * 255 / 1023) as u8, (gg * 255 / 1023) as u8, (bb * 255 / 1023) as u8]);
            }
            out
        }
        "ega" => fixed(&px, &l.ega),
        "c64" => fixed(&px, &l.c64),
        _ => fixed(&px, &l.nes),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md_levels_are_the_measured_ones() {
        let l = luts();
        assert_eq!(l.md[0], 0);
        assert_eq!(l.md[255], 255);
        assert_eq!(l.md[100], 87); // nearer 87 than 116
        assert_eq!(l.md[110], 116);
    }

    #[test]
    fn gb_has_four_greens() {
        let mut f = Vec::new();
        for v in [0u8, 90, 170, 255] {
            f.extend_from_slice(&[v, v, v]);
        }
        let out = apply(&f, 4, 1, "nope");
        assert_eq!(out, f, "unknown screens pass through");
        let mut big = vec![0u8; 160 * 144 * 3];
        big[..12].copy_from_slice(&f);
        let g = apply(&big, 160, 144, "gb");
        assert_eq!(&g[0..3], &GB_SHADES[3]);
        assert_eq!(&g[9..12], &GB_SHADES[0]);
    }

    #[test]
    fn nes_picks_palette_colours_only() {
        let pal: Vec<[u8; 3]> = NES_HEX.iter().map(|h| hex(h)).collect();
        let frame: Vec<u8> = (0..256 * 240).flat_map(|i| [(i % 251) as u8, (i % 13 * 19) as u8, (i % 7 * 36) as u8]).collect();
        let out = apply(&frame, 256, 240, "nes");
        assert!(out.as_chunks::<3>().0.iter().all(|p| pal.contains(p)));
    }
}
