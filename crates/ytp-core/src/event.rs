use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CorpusEvent {
    Installed,
    Removed,
}

/// Something that happened server-side and is fanned out to WebSocket clients.
#[derive(Debug, Clone)]
pub enum Event {
    Job {
        id: String,
        /// User the job belongs to; only they (or `jobs.view_all`) see it.
        owner: i64,
        state: JobState,
        progress: f32,
        message: Option<String>,
    },
    Corpus {
        event: CorpusEvent,
        name: String,
    },
}
