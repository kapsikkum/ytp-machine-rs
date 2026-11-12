//! Pitch detection, tracking, conversions and PSOLA / tape rendering against
//! `tests/golden/pitch.json`.

mod common;

use common::*;
use ytp_music::np::Rng;
use ytp_music::pitch::{self, Mode};
use ytp_music::SR;

const SEED: u64 = 20260913;

fn sine() -> Vec<f32> {
    (0..(0.5 * SR as f64) as usize)
        .map(|i| {
            let t = i as f64 / SR as f64;
            (0.4 * (2.0 * std::f64::consts::PI * 220.0 * t).sin()) as f32
        })
        .collect()
}

fn close(got: f64, want: f64, tol: f64, what: &str) {
    assert!((got - want).abs() <= tol, "{what}: got {got}, want {want} (tol {tol})");
}

fn check_detect(info: &pitch::PitchInfo, want: &serde_json::Value, f0_tol: f64, what: &str) {
    match want["f0"].as_f64() {
        Some(f0) => close(info.f0.expect("voiced"), f0, f0_tol, &format!("{what} f0")),
        None => assert!(info.f0.is_none(), "{what}: expected unvoiced"),
    }
    assert_eq!(info.note.as_deref(), want["note"].as_str(), "{what} note");
    close(info.voiced_ratio, want["voiced_ratio"].as_f64().unwrap(), 1e-9, &format!("{what} voiced_ratio"));
    match want["stability"].as_f64() {
        Some(s) => close(info.stability, s, 1e-6, &format!("{what} stability")),
        None => assert!(!info.stability.is_finite()),
    }
}

#[test]
fn midi_conversions() {
    let g = load_json("pitch.json");
    close(pitch::midi_to_hz(57.0), g["midi"]["midi_to_hz_57"].as_f64().unwrap(), 1e-9, "midi_to_hz");
    close(pitch::hz_to_midi(220.0), g["midi"]["hz_to_midi_220"].as_f64().unwrap(), 1e-9, "hz_to_midi");
    assert_eq!(pitch::note_name(57.0), g["midi"]["note_name_57"].as_str().unwrap());
    assert_eq!(pitch::note_name(60.0), "C4");
    assert_eq!(pitch::note_name(-3.0), "A-2");
}

#[test]
fn detect_and_track_sine() {
    let g = load_json("pitch.json");
    let x = sine();
    assert_eq!(x.len() as u64, g["input"]["samples"].as_u64().unwrap());
    let info = pitch::detect_pitch(&x, SR);
    check_detect(&info, &g["detect"], 1e-6, "sine");

    let track = pitch::track_f0_default(&x, SR);
    let voiced: Vec<f64> = track.f0.iter().zip(track.voiced.iter()).filter(|(_, v)| **v).map(|(f, _)| *f).collect();
    assert_eq!(track.f0.len() as u64, g["track"]["frames"].as_u64().unwrap());
    assert_eq!(voiced.len() as u64, g["track"]["voiced_frames"].as_u64().unwrap());
    let min = voiced.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = voiced.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    close(ytp_music::np::median_f64(&voiced), g["track"]["median_f0_voiced"].as_f64().unwrap(), 1e-6, "median");
    close(min, g["track"]["min_f0_voiced"].as_f64().unwrap(), 1e-6, "min");
    close(max, g["track"]["max_f0_voiced"].as_f64().unwrap(), 1e-6, "max");
}

#[test]
fn detect_noise_is_unvoiced() {
    let g = load_json("pitch.json");
    let mut rng = Rng::new(SEED);
    let noise: Vec<f32> = (0..SR as usize / 2).map(|_| (rng.standard_normal() * 0.2) as f32).collect();
    let info = pitch::detect_pitch(&noise, SR);
    check_detect(&info, &g["noise"]["detect"], 1e-6, "noise");
}

#[test]
fn render_perfect_and_tape() {
    let g = load_json("pitch.json");
    let x = sine();
    let voice = pitch::Voice::new(&x, SR);
    let target = pitch::midi_to_hz(57.0);
    for (key, mode) in [("perfect", Mode::Perfect), ("tape", Mode::Tape)] {
        let want = &g["render"][key];
        let (y, warp) = voice.render(Some(target), 0.3, mode, 0.0, true);
        assert_eq!(y.len() as u64, want["stats"]["samples"].as_u64().unwrap());
        let s = stats(&y);
        close(s.peak, want["stats"]["peak"].as_f64().unwrap(), 1e-6, &format!("{key} peak"));
        close(s.rms, want["stats"]["rms"].as_f64().unwrap(), 1e-6, &format!("{key} rms"));
        close(s.dc, want["stats"]["dc"].as_f64().unwrap(), 1e-6, &format!("{key} dc"));
        close(warp.hold, want["hold"].as_f64().unwrap(), 1e-9, "hold");
        close(warp.rate, want["rate"].as_f64().unwrap(), 1e-9, "rate");
        let info = pitch::detect_pitch(&y, SR);
        check_detect(&info, &want["detect"], 1e-6, key);
    }
}
