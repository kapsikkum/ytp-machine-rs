//! A uniform way to ask any of the emulated chips for a note.
//!
//! Every chip is deterministic: the same [`NoteEvent`] always renders to the
//! same samples (the only randomness anywhere, the Mega Drive's noise-drum
//! samples, is generated from fixed seeds exactly as the Python app does).

use std::fmt;

use crate::sid::Model;
use crate::{gb, nes, opl, sid, sms, snes, ym2612};

/// One note to render.
#[derive(Clone, Debug)]
pub struct NoteEvent {
    /// Pitch in Hz. Drum voices carry their own pitch and ignore it.
    pub hz: f64,
    /// How long the key is held, in seconds. Chips may ring on past this
    /// (releases, echo); the returned buffer is as long as the note needs.
    pub duration: f64,
    /// Velocity as a 0..1 level.
    pub level: f64,
    /// Output sample rate.
    pub sample_rate: u32,
    /// General MIDI program, picks the patch on the OPL and SNES chips.
    pub program: Option<u8>,
    /// General MIDI drum key, picks the percussion patch on the OPL and SNES chips.
    pub drum_key: Option<u8>,
}

impl NoteEvent {
    pub fn new(hz: f64, duration: f64) -> Self {
        NoteEvent { hz, duration, level: 1.0, sample_rate: crate::SR, program: None, drum_key: None }
    }

    pub fn from_midi(midi: f64, duration: f64) -> Self {
        Self::new(crate::pitch::midi_to_hz(midi), duration)
    }

    /// The MIDI note number of `hz` (69 = A4 = 440 Hz), or 60 when there is no pitch.
    pub fn midi_note(&self) -> f64 {
        if self.hz > 0.0 {
            69.0 + 12.0 * (self.hz / 440.0).log2()
        } else {
            60.0
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChipError {
    UnknownVoice { chip: &'static str, voice: String },
}

impl fmt::Display for ChipError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChipError::UnknownVoice { chip, voice } => write!(f, "{chip} has no voice named {voice:?}"),
        }
    }
}

impl std::error::Error for ChipError {}

/// A sound chip. `render` returns mono float32 samples at `note.sample_rate`.
pub trait Chip: Send + Sync {
    /// Short machine id: `nes`, `gb`, `sms`, `sid`, `opl2`, `opl3`, `md`, `snes`.
    fn id(&self) -> &'static str;
    /// Human name.
    fn name(&self) -> &'static str;
    /// The voices this chip offers (tone names).
    fn voices(&self) -> Vec<&'static str>;
    /// Render *voice* playing *note*.
    fn render(&self, voice: &str, note: &NoteEvent) -> Result<Vec<f32>, ChipError>;
}

pub struct Nes;
pub struct GameBoy;
pub struct Sms;
pub struct Sid {
    pub model: Model,
}
pub struct Opl {
    pub opl3: bool,
}
pub struct Ym2612;
pub struct Snes;

fn unknown(chip: &'static str, voice: &str) -> ChipError {
    ChipError::UnknownVoice { chip, voice: voice.to_string() }
}

impl Chip for Nes {
    fn id(&self) -> &'static str {
        "nes"
    }
    fn name(&self) -> &'static str {
        "NES 2A03"
    }
    fn voices(&self) -> Vec<&'static str> {
        nes::VOICES.iter().map(|v| v.name).collect()
    }
    fn render(&self, voice: &str, n: &NoteEvent) -> Result<Vec<f32>, ChipError> {
        let v = nes::voice(voice).ok_or_else(|| unknown("nes", voice))?;
        Ok(nes::render_note(v, n.hz, n.duration, n.sample_rate, n.level))
    }
}

impl Chip for GameBoy {
    fn id(&self) -> &'static str {
        "gb"
    }
    fn name(&self) -> &'static str {
        "Game Boy"
    }
    fn voices(&self) -> Vec<&'static str> {
        gb::VOICES.iter().map(|v| v.name).collect()
    }
    fn render(&self, voice: &str, n: &NoteEvent) -> Result<Vec<f32>, ChipError> {
        let v = gb::voice(voice).ok_or_else(|| unknown("gb", voice))?;
        Ok(gb::render_note(v, n.hz, n.duration, n.sample_rate, n.level))
    }
}

impl Chip for Sms {
    fn id(&self) -> &'static str {
        "sms"
    }
    fn name(&self) -> &'static str {
        "Master System SN76489"
    }
    fn voices(&self) -> Vec<&'static str> {
        sms::VOICES.iter().map(|v| v.name).collect()
    }
    fn render(&self, voice: &str, n: &NoteEvent) -> Result<Vec<f32>, ChipError> {
        let v = sms::voice(voice).ok_or_else(|| unknown("sms", voice))?;
        Ok(sms::render_note(v, n.hz, n.duration, n.sample_rate, n.level))
    }
}

impl Chip for Sid {
    fn id(&self) -> &'static str {
        if self.model == Model::Mos6581 {
            "sid"
        } else {
            "sid8580"
        }
    }
    fn name(&self) -> &'static str {
        "Commodore 64 SID"
    }
    fn voices(&self) -> Vec<&'static str> {
        sid::VOICES.iter().map(|v| v.name).collect()
    }
    fn render(&self, voice: &str, n: &NoteEvent) -> Result<Vec<f32>, ChipError> {
        let v = sid::voice(voice).ok_or_else(|| unknown("sid", voice))?;
        Ok(sid::render_note(v, n.hz, n.duration, n.sample_rate, n.level, self.model, 0.3))
    }
}

impl Chip for Opl {
    fn id(&self) -> &'static str {
        if self.opl3 {
            "opl3"
        } else {
            "opl2"
        }
    }
    fn name(&self) -> &'static str {
        if self.opl3 {
            "OPL3 (YMF262)"
        } else {
            "OPL2 (YM3812)"
        }
    }
    fn voices(&self) -> Vec<&'static str> {
        vec![self.id()]
    }
    /// The voice is the chip itself; the patch comes from `program` / `drum_key`.
    fn render(&self, voice: &str, n: &NoteEvent) -> Result<Vec<f32>, ChipError> {
        if voice != self.id() {
            return Err(unknown(self.id(), voice));
        }
        let inst = opl::instrument_for(n.program, n.drum_key);
        Ok(opl::render_note(inst, n.midi_note(), n.duration, n.sample_rate, n.level, self.opl3, 0.25))
    }
}

impl Chip for Ym2612 {
    fn id(&self) -> &'static str {
        "md"
    }
    fn name(&self) -> &'static str {
        "Mega Drive YM2612"
    }
    fn voices(&self) -> Vec<&'static str> {
        ym2612::patches().iter().map(|p| p.name).collect()
    }
    fn render(&self, voice: &str, n: &NoteEvent) -> Result<Vec<f32>, ChipError> {
        let p = ym2612::patch_by_name(voice).ok_or_else(|| unknown("md", voice))?;
        Ok(ym2612::render_note(p, n.hz, n.duration, n.sample_rate, n.level, 0.18))
    }
}

impl Chip for Snes {
    fn id(&self) -> &'static str {
        "snes"
    }
    fn name(&self) -> &'static str {
        "SNES S-DSP"
    }
    fn voices(&self) -> Vec<&'static str> {
        vec!["snes"]
    }
    fn render(&self, voice: &str, n: &NoteEvent) -> Result<Vec<f32>, ChipError> {
        if voice != "snes" {
            return Err(unknown("snes", voice));
        }
        let inst = snes::instrument_for(n.program, n.drum_key);
        let note = n.drum_key.map(|k| k as f64).unwrap_or_else(|| n.midi_note());
        Ok(snes::render_note(inst, note, n.duration, n.sample_rate, n.level, 0.35))
    }
}

/// The chip for a machine id (`nes`, `gb`, `gbc`, `sms`, `sid`, `sid8580`, `opl2`, `opl3`, `md`, `snes`).
pub fn chip_for(machine: &str) -> Option<Box<dyn Chip>> {
    Some(match machine {
        "nes" => Box::new(Nes),
        "gb" | "gbc" => Box::new(GameBoy),
        "sms" => Box::new(Sms),
        "sid" => Box::new(Sid { model: Model::Mos6581 }),
        "sid8580" => Box::new(Sid { model: Model::Mos8580 }),
        "opl2" => Box::new(Opl { opl3: false }),
        "opl3" => Box::new(Opl { opl3: true }),
        "md" => Box::new(Ym2612),
        "snes" => Box::new(Snes),
        _ => return None,
    })
}

/// Every chip, one per machine id.
pub fn all_chips() -> Vec<Box<dyn Chip>> {
    ["nes", "gb", "sms", "sid", "opl2", "opl3", "md", "snes"].iter().filter_map(|m| chip_for(m)).collect()
}
