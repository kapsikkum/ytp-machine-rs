//! A chat bot for the YTP machine that is not tied to one chat network.
//!
//! The split is deliberate:
//!
//! * [`commands`] turns a message into a [`Command`](commands::Command), exactly
//!   like the Python bot did.
//! * [`policy`] decides whether a chat user may talk to the bot at all, and
//!   which app roles they speak as.
//! * [`core`] runs the flow: permissions, rate limits, the server job, the
//!   progress message and the finished upload.
//! * [`backend`] is everything the core needs from the video server;
//!   [`transport`] everything it needs from the chat network.
//! * [`matrix`] and [`discord`] are adapters over [`transport`].
//!
//! Adapters are off-by-default only in the sense that they are features; the
//! default feature set builds both into the server binary.

pub mod backend;
pub mod commands;
pub mod core;
pub mod error;
pub mod policy;
pub mod settings;
pub mod status;
pub mod transport;

#[cfg(feature = "discord")]
pub mod discord;
#[cfg(feature = "matrix")]
pub mod matrix;

pub use backend::{
    AppIdentity, BotBackend, BotResult, BotStatus, EncodedFile, FinishedJob, JobProgress, MidiFile,
    MidiRef, PartChoice, QueueStatus, SongInfo, SongPart, Voice,
};
pub use commands::{parse, Command, CommandName, DEFAULT_PREFIX, HELP};
pub use core::{BotCore, CoreConfig};
pub use error::BotError;
pub use policy::{BotPolicy, DiscordAccess, MatrixAccess};
pub use settings::{DiscordSettings, MatrixSettings, Settings};
pub use status::StatusHandle;
pub use transport::{ChatEvent, ChatTransport, Platform};
