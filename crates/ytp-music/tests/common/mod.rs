#![allow(dead_code)]
//! Shared helpers for the parity tests: golden file loading and sample comparison.

use std::path::PathBuf;

use ytp_music::tone::{self, ToneParams};
use ytp_music::{pitch, wav, SR};

pub const NOTE_SEQ: [i32; 4] = [48, 55, 60, 64];
pub const NOTE_SECONDS: f64 = 0.18;
pub const INPUT_PEAK: f32 = 0.7;

pub fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("tests").join("golden")
}

pub fn load_wav(name: &str) -> wav::Wav {
    let path = golden_dir().join("audio").join(format!("{name}.wav"));
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    wav::read(&bytes).expect("golden wav")
}

pub fn load_json(name: &str) -> serde_json::Value {
    let path = golden_dir().join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).expect("golden json")
}

/// `_render_notes` of capture.py: each note is a flat 0.7 float32 buffer put through `tone.apply`.
pub fn render_notes(tone_name: &str, notes: &[i32], seconds: f64, program: Option<u8>) -> Vec<f32> {
    let n = (seconds * SR as f64).round_ties_even() as usize;
    let mut out = Vec::new();
    for &midi in notes {
        let y = vec![INPUT_PEAK; n];
        let p = ToneParams { hz: Some(pitch::midi_to_hz(midi as f64)), program, ..ToneParams::default() };
        out.extend(tone::apply(&y, tone_name, &p));
    }
    out
}

#[derive(Debug)]
pub struct Diff {
    pub len_got: usize,
    pub len_want: usize,
    pub max_err: i32,
    pub n_diff: usize,
    pub rms_err: f64,
    pub first_bad: Option<usize>,
}

pub fn diff_i16(got: &[i16], want: &[i16]) -> Diff {
    let n = got.len().min(want.len());
    let (mut max_err, mut n_diff, mut sq) = (0i32, 0usize, 0.0f64);
    let mut first_bad = None;
    for i in 0..n {
        let e = (got[i] as i32 - want[i] as i32).abs();
        if e != 0 {
            n_diff += 1;
            first_bad.get_or_insert(i);
        }
        max_err = max_err.max(e);
        sq += (e * e) as f64;
    }
    Diff { len_got: got.len(), len_want: want.len(), max_err, n_diff, rms_err: (sq / n.max(1) as f64).sqrt(), first_bad }
}

pub struct Stats {
    pub peak: f64,
    pub rms: f64,
    pub dc: f64,
}

pub fn stats(x: &[f32]) -> Stats {
    let a: Vec<f64> = x.iter().map(|v| *v as f64).collect();
    stats_f64(&a)
}

pub fn stats_f64(a: &[f64]) -> Stats {
    let n = a.len() as f64;
    let peak = a.iter().fold(0.0f64, |m, v| m.max(v.abs()));
    let rms = (a.iter().map(|v| v * v).sum::<f64>() / n).sqrt();
    let dc = a.iter().sum::<f64>() / n;
    Stats { peak, rms, dc }
}

/// Compare a rendered buffer with `tests/golden/audio/<name>.wav` and with the
/// stats recorded in `audio.json`.
///
/// `max_lsb` is the allowed 16-bit error, `max_diff_frac` the allowed fraction
/// of samples that are not bit-identical after quantisation.
pub fn check_against_golden(name: &str, got: &[f32], max_lsb: i32, max_diff_frac: f64) -> Diff {
    let golden = load_wav(name);
    let q = wav::quantize_f32(got);
    let d = diff_i16(&q, &golden.samples);
    let frac = d.n_diff as f64 / d.len_want.max(1) as f64;
    eprintln!(
        "[{name}] samples {}/{} max_err {} LSB, {} differ ({:.4}%), rms_err {:.4} LSB, first_bad {:?}",
        d.len_got,
        d.len_want,
        d.max_err,
        d.n_diff,
        frac * 100.0,
        d.rms_err,
        d.first_bad
    );
    assert_eq!(d.len_got, d.len_want, "{name}: length");
    assert!(d.max_err <= max_lsb, "{name}: max error {} LSB > {max_lsb}", d.max_err);
    assert!(frac <= max_diff_frac, "{name}: {:.4}% of samples differ > {:.4}%", frac * 100.0, max_diff_frac * 100.0);

    // audio.json records peak/rms/dc of the float buffer (9 decimals)
    let entries = load_json("audio.json");
    let e = entries["entries"].as_array().unwrap().iter().find(|e| e["name"] == name).expect("audio.json entry");
    let s = stats(got);
    let (gp, gr, gd) = (e["peak"].as_f64().unwrap(), e["rms"].as_f64().unwrap(), e["dc"].as_f64().unwrap());
    assert!((s.peak - gp).abs() < 2e-9, "{name}: peak {} vs {}", s.peak, gp);
    assert!((s.rms - gr).abs() < 2e-9, "{name}: rms {} vs {}", s.rms, gr);
    assert!((s.dc - gd).abs() < 2e-9, "{name}: dc {} vs {}", s.dc, gd);
    d
}
