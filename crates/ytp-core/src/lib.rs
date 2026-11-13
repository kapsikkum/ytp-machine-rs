//! Types shared by every ytp-machine crate: error type, event bus
//! payloads and the timeline (project) model.

pub mod error;
pub mod event;
pub mod history;
pub mod time;
pub mod timeline;

pub use error::{Error, Result};
pub use event::{CorpusEvent, Event, JobState};
pub use timeline::{
    Caption, Clip, ClipSource, Effect, Generator, GeneratorKind, Project, SynthNote, Track, TrackKind,
    PROJECT_VERSION,
};
