//! What a part is played through, once its notes are already right
//! (port of the routing in `app/ytpmv/tone.py`).
//!
//! A tone is either the recorded voice put through a console's sample channel
//! (`dac`, `dpcm`, `psg-pcm`, `brr`, `gb-pcm`, `sb-pcm`, `sb16-pcm`,
//! `sid-digi`), or one of the emulated chips synthesising the note.

use std::collections::HashSet;
use std::sync::OnceLock;

use crate::sid::Model;
use crate::{gb, nes, np, opl, sid, sms, snes, ym2612};

pub const DAC_RATE: f64 = 13300.0;

/// Names that used to mean something else.
pub const ALIASES: &[(&str, &str)] = &[("slap", "bass"), ("megadrive", "bass"), ("voice", "clean")];

const FIXED_TONES: &[(&str, &str)] = &[
    ("clean", "as recorded"),
    ("dac", "the voice off the Mega Drive's 8-bit sample channel"),
    ("dpcm", "the voice off the NES's delta-modulation channel"),
    ("psg-pcm", "the voice hammered out of the Master System's volume register"),
    ("brr", "the voice through the SNES's sampler, grain and echo and all"),
    ("gb-pcm", "the voice pushed through a Game Boy's wave table, four bits a step"),
    ("sb-pcm", "the voice off an original Sound Blaster's eight-bit DAC"),
    ("sb16-pcm", "the voice off a sixteen-bit Sound Blaster"),
    ("sid-digi", "the voice hammered out of the SID's volume register"),
    ("opl2", "the OPL2 playing Doom's GENMIDI patch for the part's instrument"),
    ("opl3", "the OPL3 playing the same patches, with all eight waveforms"),
    ("snes", "the SNES playing a sampled instrument, one short BRR sample of it"),
];

/// Every tone a part may be set to, with its description.
pub fn tones() -> &'static [(String, String)] {
    static T: OnceLock<Vec<(String, String)>> = OnceLock::new();
    T.get_or_init(|| {
        let mut v: Vec<(String, String)> =
            FIXED_TONES.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        v.extend(ym2612::patches().iter().map(|p| (p.name.to_string(), p.about.to_string())));
        v.extend(nes::VOICES.iter().map(|p| (p.name.to_string(), p.about.to_string())));
        v.extend(sms::VOICES.iter().map(|p| (p.name.to_string(), p.about.to_string())));
        v.extend(gb::VOICES.iter().map(|p| (p.name.to_string(), p.about.to_string())));
        v.extend(sid::VOICES.iter().map(|p| (p.name.to_string(), p.about.to_string())));
        v
    })
}

/// The tones that replace the voice rather than colour it.
pub fn synth_tones() -> &'static HashSet<String> {
    static S: OnceLock<HashSet<String>> = OnceLock::new();
    S.get_or_init(|| {
        let mut s: HashSet<String> = HashSet::new();
        s.extend(ym2612::patches().iter().map(|p| p.name.to_string()));
        s.extend(nes::VOICES.iter().map(|p| p.name.to_string()));
        s.extend(sms::VOICES.iter().map(|p| p.name.to_string()));
        s.extend(gb::VOICES.iter().map(|p| p.name.to_string()));
        s.extend(sid::VOICES.iter().map(|p| p.name.to_string()));
        s.extend(["opl2", "opl3", "snes"].map(String::from));
        s
    })
}

/// The tone *name* means, or `None` when it is not one.
pub fn resolve(name: &str) -> Option<String> {
    let name = name.trim().to_lowercase();
    if name.is_empty() {
        return None;
    }
    let name = ALIASES.iter().find(|(a, _)| *a == name).map(|(_, b)| b.to_string()).unwrap_or(name);
    tones().iter().any(|(t, _)| *t == name).then_some(name)
}

/// Everything `apply` needs besides the buffer.
#[derive(Clone, Debug)]
pub struct ToneParams {
    pub sample_rate: u32,
    /// The note, where the part has one.
    pub hz: Option<f64>,
    pub level: f64,
    /// The part's General MIDI program, which picks an OPL / SNES patch.
    pub program: Option<u8>,
    /// The part's drum key.
    pub key: Option<u8>,
    /// SID chip model.
    pub model: Model,
}

impl Default for ToneParams {
    fn default() -> Self {
        ToneParams { sample_rate: crate::SR, hz: None, level: 1.0, program: None, key: None, model: Model::Mos6581 }
    }
}

fn scaled(x: &[f32], peak: f32) -> Vec<f32> {
    x.iter().map(|v| *v * peak).collect()
}

fn midi_of(hz: Option<f64>) -> f64 {
    match hz {
        Some(h) if h != 0.0 => 69.0 + 12.0 * (h / 440.0).log2(),
        _ => 60.0,
    }
}

/// *y* played through *tone*. `clean` and anything unknown pass through.
///
/// For the synthesised tones only the length and peak of *y* are kept: how
/// long the note went on and how loud it was.
pub fn apply(y: &[f32], tone: &str, p: &ToneParams) -> Vec<f32> {
    let tone = resolve(tone).unwrap_or_else(|| "clean".to_string());
    if tone == "clean" || y.is_empty() {
        return y.to_vec();
    }
    let peak = np::abs_max_f32(y);
    if peak <= 0.0 {
        return y.to_vec();
    }
    let sr = p.sample_rate;
    let dur = y.len() as f64 / sr as f64;
    let norm = || -> Vec<f32> { y.iter().map(|v| *v / peak).collect() };

    match tone.as_str() {
        "snes" => {
            let inst = snes::instrument_for(p.program, p.key);
            let note = p.key.map(|k| k as f64).unwrap_or_else(|| midi_of(p.hz));
            return scaled(&snes::render_note(inst, note, dur, sr, p.level, 0.35), peak);
        }
        "opl2" | "opl3" => {
            let inst = opl::instrument_for(p.program, p.key);
            let x = opl::render_note(inst, midi_of(p.hz), dur, sr, p.level, tone == "opl3", 0.25);
            return scaled(&x, peak);
        }
        _ => {}
    }
    if synth_tones().contains(&tone) {
        let hz = p.hz.filter(|h| *h != 0.0).unwrap_or(220.0);
        let x = if let Some(v) = nes::voice(&tone) {
            nes::render_note(v, hz, dur, sr, p.level)
        } else if let Some(v) = sms::voice(&tone) {
            sms::render_note(v, hz, dur, sr, p.level)
        } else if let Some(v) = gb::voice(&tone) {
            gb::render_note(v, hz, dur, sr, p.level)
        } else if let Some(v) = sid::voice(&tone) {
            sid::render_note(v, hz, dur, sr, p.level, p.model, 0.3)
        } else {
            ym2612::render_note(ym2612::patch_by_name(&tone).expect("synth tone"), hz, dur, sr, p.level, 0.18)
        };
        return scaled(&x, peak);
    }
    match tone.as_str() {
        "dpcm" => {
            let x = nes::dpcm(&norm(), 10, sr);
            x.iter().map(|v| (*v / 64.0f32 - 1.0f32) * peak).collect()
        }
        "psg-pcm" => scaled(&sms::pcm(&norm(), sr), peak),
        "sb-pcm" | "sb16-pcm" => scaled(&opl::pcm(&norm(), sr, tone == "sb16-pcm"), peak),
        "sid-digi" => scaled(&sid::digi(&norm(), sr, p.model), peak),
        "gb-pcm" => scaled(&gb::pcm(&norm(), sr), peak),
        "brr" => scaled(&snes::play(&norm(), sr), peak),
        _ => scaled(&ym2612::dac(&norm(), DAC_RATE, sr, ym2612::LADDER_GAP), peak),
    }
}
