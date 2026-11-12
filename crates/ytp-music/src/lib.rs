//! Pure-Rust port of the Python app's `app/ytpmv` package: the emulated sound
//! chips (NES, Game Boy, Master System, SID, OPL2/OPL3, YM2612, SNES), pitch
//! tracking and PSOLA, beatbox drum shaping, loudness mastering and MIDI
//! analysis.
//!
//! Everything is deterministic. The public entry points are:
//!
//! * [`chip`] — the [`chip::Chip`] trait and [`chip::NoteEvent`]: ask any chip for a note
//! * [`tone`] — the Python `tone.apply` routing (synthesise, or put the voice
//!   through a console's sample channel), channel limits and allocation
//! * [`pitch`] — YIN tracking, pitch marks and [`pitch::Voice`] re-synthesis
//! * [`drums`] — turning a spoken word into a drum hit
//! * [`master`] — BS.1770 loudness, balancing, the lookahead limiter
//! * [`music`] — MIDI files into parts, roles, key and tempo
//! * [`wav`] — 16-bit WAV reading and writing
//!
//! The per-module `render_note` functions are exact ports of the Python
//! synthesis; see `PARITY.md` for how closely each one reproduces the
//! captured fixtures.

#![allow(clippy::type_complexity, clippy::too_many_arguments, clippy::vec_init_then_push)]

pub mod channels;
pub mod chip;
pub mod drums;
pub mod dsp;
pub mod gb;
pub mod master;
mod midiread;
pub mod music;
pub mod nes;
pub mod np;
pub mod opl;
pub mod pitch;
pub mod routing;
pub mod sid;
pub mod sms;
pub mod snes;
mod snes_gauss;
pub mod tone;
pub mod util;
pub mod wav;
pub mod ym2612;
mod ziggurat;

pub use chip::{Chip, ChipError, NoteEvent};

/// The sample rate everything defaults to.
pub const SR: u32 = 44100;
