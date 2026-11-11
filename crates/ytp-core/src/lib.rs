//! Types shared by every ytp-machine crate: the error type, the event bus
//! payloads and the (placeholder) timeline model.

pub mod error;
pub mod event;
pub mod time;
pub mod timeline;

pub use error::{Error, Result};
pub use event::{CorpusEvent, Event, JobState};
pub use timeline::{Clip, Project, Track, TrackKind};
