//! What the core needs from a chat network: send text, edit it, fetch an
//! attachment and upload the finished video. Implemented by the Matrix and
//! Discord adapters, and by fakes in tests.

use async_trait::async_trait;

use crate::backend::{BotResult, EncodedFile, MidiFile, MidiRef};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Matrix,
    Discord,
}

/// A message that arrived from a chat network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatEvent {
    pub platform: Platform,
    /// Room id (Matrix) or channel id (Discord).
    pub room: String,
    pub user_id: String,
    pub user_name: String,
    /// A one-to-one room, where the prefix is optional.
    pub direct: bool,
    pub text: String,
    /// The event/message id, for replies and reactions.
    pub event_id: Option<String>,
    /// The event id this message replies to.
    pub reply_to: Option<String>,
    /// Set when this very message carried a MIDI attachment.
    pub midi: Option<MidiRef>,
    /// Discord role ids, empty elsewhere.
    pub role_ids: Vec<String>,
    /// Discord guild id, `None` elsewhere or in DMs.
    pub guild: Option<String>,
}

#[async_trait]
pub trait ChatTransport: Send + Sync {
    /// The encode preset this network needs (Matrix's upload cap, Discord's
    /// guild tier).
    fn upload_preset(&self) -> String;

    /// Send a notice; the returned id can be passed to [`Self::edit`].
    async fn send(&self, room: &str, text: &str, reply_to: Option<&str>) -> BotResult<String>;

    /// Replace the text of a message sent with [`Self::send`].
    async fn edit(&self, room: &str, message_id: &str, text: &str) -> BotResult<()>;

    /// Acknowledge a command with an emoji; failures are not worth reporting.
    async fn react(&self, room: &str, message_id: &str, key: &str);

    /// Fetch the bytes of a remembered MIDI attachment.
    async fn fetch_midi(&self, midi: &MidiRef) -> BotResult<MidiFile>;

    /// Upload a finished video.
    async fn send_video(
        &self,
        room: &str,
        file: &EncodedFile,
        caption: &str,
        reply_to: Option<&str>,
    ) -> BotResult<()>;
}
