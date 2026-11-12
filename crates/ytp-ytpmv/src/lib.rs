//! YTPMV orchestration: a MIDI file played by a corpus.
//!
//! This is the half of `app/ytpmv` that is not synthesis (that lives in
//! `ytp-music`): corpus words cut into instruments (`samples`), the choice of
//! which word plays which part (`recommend`), per-part settings
//! (`settings`), the in-process audio mix (`mix`) and the picture — a grid of
//! clips or oscilloscopes, optionally put through a console's palette
//! (`video`, `layout`, `screen`, `scope`) — piped into one ffmpeg.
//!
//! Sound is built entirely in-process; ffmpeg is only used to decode source
//! audio/video and to encode the finished video.

#![allow(clippy::too_many_arguments, clippy::type_complexity)]

pub mod decode;
pub mod layout;
pub mod mix;
pub mod recommend;
pub mod samples;
pub mod scope;
pub mod screen;
pub mod settings;
pub mod song;
pub mod video;

use std::fmt;

pub use ytp_music::SR;

/// Frames per second of the picture.
pub const FPS: usize = 25;
/// Width and height of the finished video.
pub const OUT_W: usize = 1920;
pub const OUT_H: usize = 1080;
/// Source video is 480x270.
pub const SRC_W: usize = 480;
pub const SRC_H: usize = 270;
/// The longest song rendered, in seconds.
pub const MAX_SECONDS: f64 = 900.0;
/// Let the last notes ring out after the final note-off.
pub const TAIL: f64 = 0.8;

#[derive(Debug)]
pub enum Error {
    /// The request was cancelled (progress callback returned `false`).
    Cancelled,
    /// Something about the request the person making it can fix, or a source
    /// that cannot be read.
    Msg(String),
    NotFound(String),
    Internal(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Cancelled => f.write_str("cancelled"),
            Error::Msg(m) | Error::NotFound(m) | Error::Internal(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub fn msg<T>(m: impl Into<String>) -> Result<T> {
    Err(Error::Msg(m.into()))
}

/// `progress(stage, done, total)`; return `false` to cancel.
pub type Progress<'a> = &'a (dyn Fn(&str, usize, usize) -> bool + Sync);

pub(crate) fn say(progress: Progress, stage: &str, done: usize, total: usize) -> Result<()> {
    if progress(stage, done, total) {
        Ok(())
    } else {
        Err(Error::Cancelled)
    }
}

/// Python's `round(x, n)` (half to even on the scaled value).
pub fn round_to(x: f64, digits: i32) -> f64 {
    let k = 10f64.powi(digits);
    (x * k).round_ties_even() / k
}
