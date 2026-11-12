//! Chip parity against `tests/golden/audio/*.wav` (rendered by the Python app
//! through `tone.apply`, see `tools/golden/capture.py`).

mod common;

use common::*;

fn chip(name: &str, tone: &str, max_lsb: i32, max_diff_frac: f64) {
    let got = render_notes(tone, &NOTE_SEQ, NOTE_SECONDS, None);
    check_against_golden(name, &got, max_lsb, max_diff_frac);
}

#[test]
fn nes() {
    chip("nes", "pulse", 0, 0.0);
}

#[test]
fn gb() {
    chip("gb", "gb-pulse", 0, 0.0);
}

#[test]
fn sms() {
    chip("sms", "psg", 0, 0.0);
}

#[test]
fn sid() {
    chip("sid", "sid-lead", 0, 0.0);
}

#[test]
fn opl2() {
    let got = render_notes("opl2", &NOTE_SEQ, NOTE_SECONDS, Some(0));
    check_against_golden("opl2", &got, 0, 0.0);
}

#[test]
fn opl3() {
    let got = render_notes("opl3", &NOTE_SEQ, NOTE_SECONDS, Some(0));
    check_against_golden("opl3", &got, 0, 0.0);
}

#[test]
fn ym2612() {
    chip("ym2612", "lead", 0, 0.0);
}

#[test]
fn snes() {
    let mut got = render_notes("snes", &[48, 60], 0.05, Some(0));
    got.truncate(44100);
    check_against_golden("snes", &got, 0, 0.0);
}
