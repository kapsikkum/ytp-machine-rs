//! Which tone plays which part when a whole song goes through one chip
//! (the pure helpers of `app/ytpmv/render.py`: `which_chip`, `chip_tone`,
//! `bass_line`, `floor_hz`, `octaves_to_fit`).

use crate::channels::sampled_tone;
use crate::music::{Note, Part, Song};
use crate::pitch::midi_to_hz;
use crate::{gb, nes, sid, sms, ym2612};

/// Every chip setting a song can be played through, including `-voice` variants.
pub const CHIPS: &[&str] = &["md", "nes", "sms", "snes", "gb", "gbc", "opl2", "opl3", "sid", "sid8580"];

/// Which machine *asked* means, or `None` for none.
pub fn which_chip(asked: &str) -> Option<String> {
    let name = asked.trim().to_lowercase();
    let name = match name.as_str() {
        "megadrive" => "md".to_string(),
        "megadrive-voice" => "md-voice".to_string(),
        "genesis" | "true" | "on" | "yes" => "md".to_string(),
        other => other.to_string(),
    };
    let machine = name.split('-').next().unwrap_or("");
    let ok = CHIPS.contains(&machine) && (name == machine || name == format!("{machine}-voice"));
    ok.then_some(name)
}

/// The synthesising setting to play a song on when there is no voice.
pub fn synth_chip(chip: Option<&str>) -> String {
    chip.unwrap_or("md").split('-').next().unwrap_or("md").to_string()
}

/// The part to treat as the bass when nothing in the song is called one.
pub fn bass_line(song: &Song) -> Option<String> {
    let melodic: Vec<&Part> = song.parts.iter().filter(|p| !p.is_drums() && !p.notes.is_empty()).collect();
    if melodic.is_empty() || melodic.iter().any(|p| p.role == "bass") {
        return None;
    }
    let mut low = melodic[0];
    for p in &melodic {
        if p.median_pitch() < low.median_pitch() {
            low = p;
        }
    }
    (low.role != "lead" && low.median_pitch() < 53.0).then(|| low.id.clone())
}

/// What *part* is played by when the whole song goes through *chip*.
pub fn chip_tone(part: &Part, chip: &str, as_bass: bool, program: Option<u8>, voice_drums: bool) -> String {
    let (machine, mode) = match chip.split_once('-') {
        Some((m, mode)) => (m, mode),
        None => (chip, ""),
    };
    let program = if program.is_none() || part.is_drums() { part.program } else { program };
    if mode == "voice" || (part.is_drums() && voice_drums) {
        return sampled_tone(machine).unwrap_or("dac").to_string();
    }
    let role = if as_bass && !part.is_drums() { "bass" } else { part.role.as_str() };
    match machine {
        "nes" => nes::voice_for(role, program).name.to_string(),
        "sms" => sms::voice_for(role, program).name.to_string(),
        "gb" | "gbc" => gb::voice_for(role, program).name.to_string(),
        "opl2" | "opl3" | "snes" => machine.to_string(),
        "sid" | "sid8580" => sid::voice_for(role, program).name.to_string(),
        _ => ym2612::patch_for(role, program).name.to_string(),
    }
}

/// The lowest note *tone* can play, or 0 when it has no such limit.
pub fn floor_hz(tone: &str) -> f64 {
    if let Some(v) = nes::voice(tone) {
        return nes::floor_hz(v);
    }
    if let Some(v) = sms::voice(tone) {
        return sms::floor_hz(v);
    }
    if let Some(v) = gb::voice(tone) {
        return if v.base_hz != 0.0 { 0.0 } else { gb::floor_hz(v.kind) };
    }
    0.0
}

/// Whole octaves to raise a part by so its lowest note is not under *floor*.
pub fn octaves_to_fit(notes: &[Note], floor: f64, shift: i32) -> i32 {
    if floor <= 0.0 || notes.is_empty() {
        return 0;
    }
    let lowest = midi_to_hz(notes.iter().map(|n| n.pitch as i32).min().unwrap() as f64 + shift as f64);
    let mut up = 0;
    while lowest * 2.0f64.powi(up) < floor - 1e-6 && up < 6 {
        up += 1;
    }
    up
}
