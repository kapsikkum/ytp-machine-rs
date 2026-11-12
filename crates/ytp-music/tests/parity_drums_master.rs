//! Drums (chip drum kits through `tone.apply`) and `master.finish` parity.

mod common;

use common::*;
use ytp_music::tone::{self, ToneParams};
use ytp_music::{master, wav, SR};

#[test]
fn drums_pattern() {
    let pattern: [(&str, u8, f64); 6] = [
        ("md-kick", 36, 0.15),
        ("nes-snare", 38, 0.12),
        ("sms-hat", 42, 0.08),
        ("gb-kick", 36, 0.12),
        ("sid-cymbal", 49, 0.15),
        ("md-snare", 38, 0.12),
    ];
    let mut out = Vec::new();
    for (tone_name, key, seconds) in pattern {
        let n = (seconds * SR as f64).round_ties_even() as usize;
        let y = vec![0.8f32; n];
        let p = ToneParams { hz: None, key: Some(key), ..ToneParams::default() };
        out.extend(tone::apply(&y, tone_name, &p));
    }
    check_against_golden("drums", &out, 0, 0.0);
}

fn master_input() -> Vec<f64> {
    let n = (0.5 * SR as f64) as usize;
    let mut mix = vec![0.0f64; n * 2];
    for i in 0..n {
        let t = i as f64 / SR as f64;
        mix[2 * i] = 0.30 * (2.0 * std::f64::consts::PI * 220.0 * t).sin();
        mix[2 * i + 1] = 0.30 * (2.0 * std::f64::consts::PI * 330.0 * t).sin();
    }
    for i in 2000..2050 {
        mix[2 * i] = 0.99;
        mix[2 * i + 1] = 0.99;
    }
    mix
}

#[test]
fn master_finish() {
    let mut mix = master_input();
    let pre = stats_f64(&mix);
    master::finish(&mut mix, 2, SR);
    let golden = load_wav("master");
    let q: Vec<i16> = mix.iter().map(|v| wav::quantize(*v)).collect();
    let d = diff_i16(&q, &golden.samples);
    eprintln!("[master] {d:?}");
    assert_eq!(d.len_got, d.len_want);

    let entries = load_json("audio.json");
    let e = entries["entries"].as_array().unwrap().iter().find(|e| e["name"] == "master").unwrap();
    let post = stats_f64(&mix);
    assert!((pre.peak - e["peak_before"].as_f64().unwrap()).abs() < 1e-9);
    assert!((pre.rms - e["rms_before"].as_f64().unwrap()).abs() < 1e-9);
    assert!((post.peak - e["peak"].as_f64().unwrap()).abs() < 1e-6, "peak {}", post.peak);
    assert!((post.rms - e["rms"].as_f64().unwrap()).abs() < 1e-6, "rms {}", post.rms);
    assert!((post.dc - e["dc"].as_f64().unwrap()).abs() < 1e-6, "dc {}", post.dc);
    let lufs = master::loudness(&mix, 2, SR);
    assert!((lufs - e["loudness_lufs"].as_f64().unwrap()).abs() < 1e-3, "lufs {lufs}");
    assert_eq!(master::TARGET_LUFS, e["target_lufs"].as_f64().unwrap());
    assert_eq!(master::CEILING, e["ceiling"].as_f64().unwrap());
    assert!(d.max_err == 0, "max err {} LSB", d.max_err);
}
