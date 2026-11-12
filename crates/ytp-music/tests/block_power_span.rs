//! `block_power_span()` measures a stem without storing the silence around it;
//! it must give exactly what `block_power()` gives for the zero-padded signal,
//! including when the stem crosses the 60 s segment boundary the loop works in.

use ytp_music::master::{block_power, block_power_span};

const SR: u32 = 8000;

fn stem(frames: usize, seed: u32) -> Vec<f32> {
    let mut s = seed;
    (0..frames)
        .map(|i| {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (s >> 16) as f32 / 65_536.0 - 0.5;
            (i as f32 * 0.07).sin() * 0.4 + noise * 0.2
        })
        .collect()
}

fn padded(stem: &[f32], lo: usize, frames: usize) -> Vec<f64> {
    let mut x = vec![0.0f64; frames];
    for (i, v) in stem.iter().enumerate() {
        x[lo + i] = *v as f64;
    }
    x
}

fn check(lo: usize, len: usize, frames: usize) {
    let s = stem(len, lo as u32 + 7);
    let want = block_power(&padded(&s, lo, frames), 1, SR);
    let got = block_power_span(&s, lo, frames, SR);
    assert_eq!(got.len(), want.len(), "lo={lo} len={len} frames={frames}");
    for (i, (a, b)) in got.iter().zip(&want).enumerate() {
        assert!((a - b).abs() <= 1e-12 * b.abs().max(1.0), "block {i}: {a} vs {b} (lo={lo} len={len})");
    }
}

#[test]
fn span_matches_padded_inside_one_segment() {
    check(2 * SR as usize, 5 * SR as usize, 20 * SR as usize);
}

#[test]
fn span_matches_padded_across_the_sixty_second_boundary() {
    // 55 s .. 70 s of sound in a 150 s signal: the stem straddles the 60 s step.
    check(55 * SR as usize, 15 * SR as usize, 150 * SR as usize);
}

#[test]
fn span_matches_padded_with_stem_at_the_edges() {
    check(0, 3 * SR as usize, 90 * SR as usize);
    check(87 * SR as usize, 3 * SR as usize, 90 * SR as usize);
}

#[test]
fn silent_stem_has_no_power() {
    let s = vec![0.0f32; 4 * SR as usize];
    assert!(block_power_span(&s, SR as usize, 70 * SR as usize, SR).iter().all(|v| *v == 0.0));
}
