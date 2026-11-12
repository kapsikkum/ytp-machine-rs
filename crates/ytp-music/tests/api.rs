//! Public-API behaviour that is not about matching Python sample for sample.

use ytp_music::chip::{all_chips, chip_for, NoteEvent};
use ytp_music::channels;
use ytp_music::tone::{self, ToneParams};
use ytp_music::{opl, snes, wav, ym2612};

#[test]
fn every_chip_is_deterministic_and_audible() {
    for chip in all_chips() {
        let voice = chip.voices()[0];
        let mut ev = NoteEvent::from_midi(60.0, 0.1);
        ev.program = Some(0);
        let a = chip.render(voice, &ev).unwrap();
        let b = chip.render(voice, &ev).unwrap();
        assert_eq!(a, b, "{} not deterministic", chip.id());
        assert!(a.len() >= 4410, "{}: {} samples", chip.id(), a.len());
        assert!(a.iter().any(|v| v.abs() > 1e-3), "{} is silent", chip.id());
        assert!(a.iter().all(|v| v.is_finite()), "{} produced NaN/inf", chip.id());
    }
}

#[test]
fn unknown_voice_is_an_error() {
    let chip = chip_for("nes").unwrap();
    assert!(chip.render("no-such-voice", &NoteEvent::new(220.0, 0.1)).is_err());
    assert!(chip_for("zx-spectrum").is_none());
}

#[test]
fn every_voice_of_every_chip_renders() {
    for chip in all_chips() {
        for voice in chip.voices() {
            let mut ev = NoteEvent::from_midi(57.0, 0.08);
            ev.program = Some(3);
            let out = chip.render(voice, &ev).unwrap_or_else(|e| panic!("{}/{voice}: {e}", chip.id()));
            assert!(out.iter().all(|v| v.is_finite()), "{}/{voice}", chip.id());
        }
    }
}

#[test]
fn embedded_banks_load() {
    assert_eq!(opl::bank().len(), 175);
    assert!(!opl::bank()[0].name.is_empty());
    let b = &snes::bank().instruments;
    assert_eq!(b.len(), 189);
    assert!(b.contains_key("p0") && b.contains_key("d38"));
    assert!(snes::bank().source.contains("ExpressiveSNES"));
    assert!(ym2612::patch_by_name("md-snare").is_some());
}

#[test]
fn tone_names_and_channels() {
    assert_eq!(tone::resolve(" Slap "), Some("bass".to_string()));
    assert_eq!(tone::resolve("voice"), Some("clean".to_string()));
    assert_eq!(tone::resolve("nonsense"), None);
    assert!(channels::overrides_switch(Some("voice")));
    assert!(!channels::overrides_switch(Some("clean")));
    assert!(channels::overrides_switch(Some("pulse")));
    assert_eq!(channels::channel(Some("pulse")), Some(("nes", "pulse")));
    assert_eq!(channels::channel(Some("md-snare")), Some(("md", "dac")));
    assert_eq!(channels::channel(Some("clean")), None);
    // clean passes through untouched
    let y = vec![0.5f32; 100];
    assert_eq!(tone::apply(&y, "clean", &ToneParams::default()), y);
}

#[test]
fn channel_allocation_steals_the_quietest_voice() {
    use ytp_music::music::{Note, Part};
    let note = |p: u8| Note { start: 0.0, dur: 1.0, pitch: p, velocity: 90 };
    let mk = |id: &str, role: &str, n: usize| Part {
        id: id.into(),
        name: id.into(),
        track: 0,
        channel: 0,
        program: Some(0),
        notes: (0..n).map(|i| note(60 + i as u8)).collect(),
        role: role.into(),
        drum_group: None,
        pan: None,
    };
    // the NES has two pulse channels: three simultaneous notes lose one
    let lead = mk("lead", "lead", 2);
    let pad = mk("pad", "chords", 1);
    let cuts = channels::allocate(&[(&lead, "pulse"), (&pad, "pulse")], 1.0);
    assert_eq!(cuts.len(), 1);
    assert!(cuts.keys().all(|k| k.0 == "pad"), "the lead keeps its channels: {cuts:?}");
}

#[test]
fn wav_round_trip() {
    let w = wav::Wav { sample_rate: 44100, channels: 2, samples: vec![0, 1, -1, 32767, -32768, 12345] };
    assert_eq!(wav::read(&wav::write(&w)).unwrap().samples, w.samples);
    assert!(wav::read(b"RIFFxxxxnope").is_err());
}
