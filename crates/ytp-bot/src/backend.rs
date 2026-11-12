//! What the bot needs from the video server, and the shapes that cross the
//! boundary.
//!
//! The server implements [`BotBackend`]; the chat adapters never touch the
//! job system, the corpus store or ffmpeg directly. Everything a job runs as
//! the configured *service* app user, so the server's own quotas and limits
//! apply; what the chat person may ask for is decided separately from their
//! mapped app roles.

use std::collections::BTreeSet;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ytp_auth::{Permission, Quota};

pub use crate::error::BotResult;

/// A MIDI file uploaded by a chat user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiFile {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// Where the last MIDI posted in a room lives. `key` is whatever the chat
/// adapter needs to fetch it again (a Matrix event id, a Discord URL).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiRef {
    pub name: String,
    pub key: String,
}

/// The app user a bot speaks as, with the roles and limits it resolved to.
#[derive(Debug, Clone, PartialEq)]
pub struct AppIdentity {
    pub username: String,
    pub roles: Vec<String>,
    pub permissions: BTreeSet<Permission>,
    pub quota: Quota,
}

impl AppIdentity {
    pub fn has(&self, p: Permission) -> bool {
        self.permissions.contains(&p)
    }
}

/// One part of a song, as the analysis describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct SongPart {
    pub id: String,
    pub name: String,
    pub role: String,
    pub instrument: String,
    /// The word the render would use, if the analysis suggested one.
    pub text: Option<String>,
    pub octave: i64,
}

/// What `mv info` and the part matching need to know about a song.
#[derive(Debug, Clone, PartialEq)]
pub struct SongInfo {
    pub midi_id: String,
    pub title: String,
    pub description: String,
    pub parts: Vec<SongPart>,
}

/// One part override the core resolved from `part=word` / `octave:part=+1`.
#[derive(Debug, Clone, PartialEq)]
pub struct PartChoice {
    pub id: String,
    pub settings: Value,
}

/// An installed corpus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Voice {
    pub name: String,
    pub words: Option<u64>,
    /// The voice the bot would use for the next render.
    pub active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueStatus {
    pub running: usize,
    pub queued: usize,
}

/// One poll of a running job.
#[derive(Debug, Clone, PartialEq)]
pub struct JobProgress {
    pub state: String,
    pub progress: f32,
    pub message: Option<String>,
}

/// A finished render.
#[derive(Debug, Clone, PartialEq)]
pub struct FinishedJob {
    pub job_id: String,
    /// The job record's report: `missing` for a sentence, `parts`/`problems`
    /// for a song.
    pub report: Value,
    pub duration: Option<f64>,
}

/// A file the server encoded for a chat upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFile {
    pub path: PathBuf,
    pub filename: String,
    pub mime: String,
    pub bytes: u64,
}

/// The server side of a bot. Every call is made as the service user.
#[async_trait]
pub trait BotBackend: Send + Sync {
    /// Find the configured service account.
    async fn service_identity(&self, username: &str) -> BotResult<AppIdentity>;

    /// Resolve app roles to the permissions and quota they grant.
    async fn grants(&self, roles: &[String]) -> BotResult<(BTreeSet<Permission>, Quota)>;

    /// Queue a sentence render; returns the job id.
    async fn start_ytp(
        &self,
        service: &AppIdentity,
        text: &str,
        subtitles: bool,
        voice: Option<&str>,
    ) -> BotResult<String>;

    /// Describe a song and suggest a word for each part.
    async fn ytpmv_info(
        &self,
        service: &AppIdentity,
        midi: &MidiFile,
        voice: Option<&str>,
    ) -> BotResult<SongInfo>;

    /// Queue a song render; returns the job id.
    async fn start_ytpmv(
        &self,
        service: &AppIdentity,
        midi: &MidiFile,
        parts: &[PartChoice],
        options: &Value,
        voice: Option<&str>,
    ) -> BotResult<String>;

    async fn list_voices(&self) -> BotResult<Vec<Voice>>;

    async fn switch_voice(&self, service: &AppIdentity, name: &str) -> BotResult<Voice>;

    async fn queue_status(&self) -> BotResult<QueueStatus>;

    /// Follow a job to the end, calling `on_progress` whenever something
    /// changed (the first poll included).
    async fn wait_job(
        &self,
        job_id: &str,
        on_progress: &(dyn for<'a> Fn(&'a JobProgress) + Send + Sync),
    ) -> BotResult<FinishedJob>;

    /// Encode a finished render with a named preset and return the file.
    async fn encode_result(
        &self,
        service: &AppIdentity,
        job_id: &str,
        preset: &str,
    ) -> BotResult<EncodedFile>;

    /// Called once the upload is done (the server may delete the file).
    async fn release(&self, file: &EncodedFile) {
        let _ = file;
    }
}

/// A connection state the adapters update and the admin endpoint reports.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BotStatus {
    pub connected: bool,
    pub last_error: Option<String>,
    /// RFC3339 time of the last change.
    pub since: Option<String>,
}
