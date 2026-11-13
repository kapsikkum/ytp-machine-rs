//! The Matrix adapter: a [`ChatTransport`] over `matrix-sdk`, plus the loop
//! that logs in, joins invites from allowed people and feeds messages to the
//! core.
//!
//! End-to-end encrypted rooms are not supported (the SDK's `e2e-encryption`
//! feature is deliberately off); the bot says so once in an encrypted room it
//! is in, and leaves one it is invited to, instead of sitting there looking
//! idle.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use matrix_sdk::authentication::matrix::{MatrixSession, MatrixSessionTokens};
use matrix_sdk::config::SyncSettings;
use matrix_sdk::media::{MediaFormat, MediaRequestParameters};
use matrix_sdk::ruma::events::room::encrypted::OriginalSyncRoomEncryptedEvent;
use matrix_sdk::ruma::events::room::member::{MembershipState, StrippedRoomMemberEvent};
use matrix_sdk::ruma::events::room::message::{
    MessageType, OriginalSyncRoomMessageEvent, Relation,
};
use matrix_sdk::ruma::events::room::MediaSource;
use matrix_sdk::ruma::{OwnedMxcUri, OwnedUserId};
use matrix_sdk::{Client, LoopCtrl, Room, SessionMeta};
use serde_json::json;

use crate::backend::{BotResult, EncodedFile, MidiFile, MidiRef};
use crate::commands::is_midi;
use crate::core::BotCore;
use crate::error::BotError;
use crate::settings::MatrixSettings;
use crate::status::StatusHandle;
use crate::transport::{ChatEvent, ChatTransport, Platform};

/// Matches the web app's MIDI upload limit.
const MAX_MIDI_BYTES: usize = 2 << 20;
const BACKOFF_MAX_SECS: u64 = 60;
const SYNC_TIMEOUT_SECS: u64 = 30;

/// Target bytes of the target-size presets, in the same decimal megabytes
/// as `presets.toml`. Kept here so the choice below stays pure and the bot
/// does not need the encoder just to read a number.
const MATRIX_TARGET_BYTES: u64 = 50_000_000;
const DISCORD_50MB_TARGET_BYTES: u64 = 50_000_000;
const DISCORD_25MB_TARGET_BYTES: u64 = 25_000_000;
const DISCORD_10MB_TARGET_BYTES: u64 = 10_000_000;
/// A target must leave this much room under the server's hard cap for
/// container overhead: `target * 105 / 100 <= limit`.
const UPLOAD_SAFETY_PERCENT: u64 = 105;

/// What preset the transport asks the encoder for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PresetChoice {
    /// The name of a preset that fits the limit.
    Preset(&'static str),
    /// Not even the smallest preset fits; the value is the server's cap.
    TooSmall { limit: u64 },
}

/// Does a target of `target` bytes fit under `limit` with the safety margin?
fn target_fits(target: u64, limit: u64) -> bool {
    target.saturating_mul(UPLOAD_SAFETY_PERCENT) / 100 <= limit
}

/// The preset to encode with, given the homeserver's upload cap. `None`
/// (the cap could not be read) keeps the historical Matrix preset.
fn pick_upload_preset(limit: Option<u64>) -> PresetChoice {
    let Some(limit) = limit else {
        return PresetChoice::Preset("matrix");
    };
    // Order matters: "matrix" and "discord-50mb" share a 50 MB target, and
    // the Matrix name is the right one to keep when it fits.
    const CANDIDATES: [(&str, u64); 4] = [
        ("matrix", MATRIX_TARGET_BYTES),
        ("discord-50mb", DISCORD_50MB_TARGET_BYTES),
        ("discord-25mb", DISCORD_25MB_TARGET_BYTES),
        ("discord-10mb", DISCORD_10MB_TARGET_BYTES),
    ];
    for (name, target) in CANDIDATES {
        if target_fits(target, limit) {
            return PresetChoice::Preset(name);
        }
    }
    PresetChoice::TooSmall { limit }
}

type AnyError = Box<dyn std::error::Error + Send + Sync>;

/// One Matrix connection. The core talks to the rooms through this.
pub struct MatrixTransport {
    client: Client,
    /// The homeserver's upload cap, read once after login. `None` means the
    /// server did not say, and the historical 50 MB preset is used.
    upload_limit_bytes: Option<u64>,
}

impl MatrixTransport {
    fn room(&self, room: &str) -> BotResult<Room> {
        let id = matrix_sdk::ruma::RoomId::parse(room)
            .map_err(|e| BotError::internal(format!("bad room id {room:?}: {e}")))?;
        self.client
            .get_room(&id)
            .ok_or_else(|| BotError::internal(format!("not in room {room}")))
    }
}

fn reply_relation(content: &mut serde_json::Value, reply_to: Option<&str>) {
    if let Some(id) = reply_to {
        content["m.relates_to"] = json!({ "m.in_reply_to": { "event_id": id } });
    }
}

/// Ask the homeserver for its upload cap, once. Any failure is only a
/// warning: the bot keeps working with the default preset.
async fn fetch_upload_limit(client: &Client) -> Option<u64> {
    match try_fetch_upload_limit(client).await {
        Ok(limit) => limit,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "matrix: could not read the homeserver's upload limit; using the default preset"
            );
            None
        }
    }
}

async fn try_fetch_upload_limit(client: &Client) -> Result<Option<u64>, AnyError> {
    let token = client.access_token().ok_or("not logged in")?;
    let base = client.homeserver();
    let http = reqwest::Client::new();
    // The v1 endpoint is current; older homeservers only know the /media/v3
    // one, so a 404 falls through to it.
    let mut resp = http
        .get(base.join("/_matrix/client/v1/media/config")?)
        .bearer_auth(&token)
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        resp = http
            .get(base.join("/_matrix/media/v3/config")?)
            .bearer_auth(&token)
            .send()
            .await?;
    }
    let body: serde_json::Value = resp.json().await?;
    let bytes = body.get("m.upload.size").and_then(serde_json::Value::as_u64);
    Ok(bytes.filter(|b| *b > 0))
}

#[async_trait]
impl ChatTransport for MatrixTransport {
    fn upload_preset(&self) -> String {
        match pick_upload_preset(self.upload_limit_bytes) {
            PresetChoice::Preset(name) => name.to_string(),
            PresetChoice::TooSmall { limit } => {
                // `upload_preset` cannot fail, so encode with the smallest
                // preset and let `send_video` turn a still-too-big file into
                // a chat-visible error.
                tracing::error!(
                    limit,
                    "matrix: the homeserver's upload limit fits no video preset; the upload will be refused"
                );
                "discord-10mb".to_string()
            }
        }
    }

    async fn send(&self, room: &str, text: &str, reply_to: Option<&str>) -> BotResult<String> {
        let room = self.room(room)?;
        let mut content = json!({ "msgtype": "m.notice", "body": text });
        reply_relation(&mut content, reply_to);
        let resp = room
            .send_raw("m.room.message", content)
            .await
            .map_err(|e| BotError::internal(format!("could not send a message: {e}")))?;
        Ok(resp.event_id.to_string())
    }

    async fn edit(&self, room: &str, message_id: &str, text: &str) -> BotResult<()> {
        let room = self.room(room)?;
        room.send_raw(
            "m.room.message",
            json!({
                "msgtype": "m.notice",
                "body": format!("* {text}"),
                "m.new_content": { "msgtype": "m.notice", "body": text },
                "m.relates_to": { "rel_type": "m.replace", "event_id": message_id },
            }),
        )
        .await
        .map_err(|e| BotError::internal(format!("could not edit a message: {e}")))?;
        Ok(())
    }

    async fn react(&self, room: &str, message_id: &str, key: &str) {
        let Ok(room) = self.room(room) else { return };
        let sent = room
            .send_raw(
                "m.reaction",
                json!({
                    "m.relates_to": {
                        "rel_type": "m.annotation",
                        "event_id": message_id,
                        "key": key,
                    }
                }),
            )
            .await;
        if let Err(e) = sent {
            tracing::debug!(error = %e, "could not react to a message");
        }
    }

    async fn fetch_midi(&self, midi: &MidiRef) -> BotResult<MidiFile> {
        let uri = OwnedMxcUri::from(midi.key.as_str());
        if !uri.is_valid() {
            return Err(BotError::user(format!(
                "{} has an invalid media address",
                midi.name
            )));
        }
        let bytes = self
            .client
            .media()
            .get_media_content(
                &MediaRequestParameters {
                    source: MediaSource::Plain(uri),
                    format: MediaFormat::File,
                },
                false,
            )
            .await
            .map_err(|e| BotError::user(format!("Couldn't download {}: {e}", midi.name)))?;
        if bytes.len() > MAX_MIDI_BYTES {
            return Err(BotError::user(format!(
                "{} is over {} MB",
                midi.name,
                MAX_MIDI_BYTES / (1 << 20)
            )));
        }
        Ok(MidiFile {
            name: midi.name.clone(),
            bytes,
        })
    }

    async fn send_video(
        &self,
        room: &str,
        file: &EncodedFile,
        caption: &str,
        reply_to: Option<&str>,
    ) -> BotResult<()> {
        let room = self.room(room)?;
        let data = tokio::fs::read(&file.path)
            .await
            .map_err(|e| BotError::internal(format!("could not read the video: {e}")))?;
        let size = data.len();
        if let Some(limit) = self.upload_limit_bytes {
            if size as u64 > limit {
                return Err(BotError::user(format!(
                    "The video is {} bytes, over this homeserver's upload limit of {} bytes. \
                     Try a shorter video.",
                    size, limit
                )));
            }
        }
        let mime = file
            .mime
            .parse::<mime::Mime>()
            .unwrap_or(mime::APPLICATION_OCTET_STREAM);
        let uploaded = self
            .client
            .media()
            .upload(&mime, data, None)
            .await
            .map_err(|e| BotError::user(format!("The upload failed ({e}).")))?;
        let mut content = json!({
            "msgtype": "m.video",
            "body": if caption.is_empty() { file.filename.as_str() } else { caption },
            "filename": file.filename,
            "url": uploaded.content_uri.to_string(),
            "info": { "mimetype": file.mime, "size": size },
        });
        reply_relation(&mut content, reply_to);
        room.send_raw("m.room.message", content)
            .await
            .map_err(|e| BotError::user(format!("The upload failed ({e}).")))?;
        Ok(())
    }
}

/// What the event handlers share.
struct Shared {
    core: Arc<BotCore>,
    status: Arc<StatusHandle>,
    transport: MatrixTransport,
    own: OwnedUserId,
    prefix: String,
    warned: Mutex<HashSet<String>>,
}

/// The Matrix bot: everything needed to run and reconnect it.
pub struct MatrixBot {
    settings: MatrixSettings,
    core: Arc<BotCore>,
    status: Arc<StatusHandle>,
}

impl MatrixBot {
    pub fn new(settings: MatrixSettings, core: Arc<BotCore>, status: Arc<StatusHandle>) -> Self {
        Self {
            settings,
            core,
            status,
        }
    }

    /// Run until the process stops, reconnecting with a growing pause. A chat
    /// network that is down is never allowed to take the video server with it.
    pub async fn run(self) {
        let mut backoff = 1u64;
        loop {
            let started = std::time::Instant::now();
            let outcome = self.listen().await;
            if started.elapsed() > Duration::from_secs(120) {
                backoff = 1;
            }
            match outcome {
                Ok(()) => tracing::warn!("matrix sync ended"),
                Err(e) => {
                    tracing::warn!(error = %e, "matrix bot disconnected");
                    self.status.failed(e.to_string());
                }
            }
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(BACKOFF_MAX_SECS);
        }
    }

    async fn listen(&self) -> Result<(), AnyError> {
        let client = self.connect().await?;
        let upload_limit_bytes = fetch_upload_limit(&client).await;
        let own = client
            .user_id()
            .map(ToOwned::to_owned)
            .ok_or("logged in without a user id")?;
        let shared = Arc::new(Shared {
            core: self.core.clone(),
            status: self.status.clone(),
            transport: MatrixTransport {
                client: client.clone(),
                upload_limit_bytes,
            },
            own,
            prefix: self.settings.prefix.clone(),
            warned: Mutex::new(HashSet::new()),
        });

        // Invites are handled during the catch-up sync too: the ones that
        // arrived while the bot was offline are still worth taking.
        let s = shared.clone();
        client.add_event_handler(move |ev: StrippedRoomMemberEvent, room: Room| {
            let s = s.clone();
            async move { on_invite(s, ev, room).await }
        });

        // Catch up without answering: anything sent while the bot was down is
        // history, and answering a morning's commands at once is worse than
        // answering none.
        client
            .sync_once(SyncSettings::default().timeout(Duration::ZERO).full_state(true))
            .await?;

        let s = shared.clone();
        client.add_event_handler(move |ev: OriginalSyncRoomMessageEvent, room: Room| {
            let s = s.clone();
            async move { on_message(s, ev, room).await }
        });
        let s = shared.clone();
        client.add_event_handler(move |ev: OriginalSyncRoomEncryptedEvent, room: Room| {
            let s = s.clone();
            async move { on_encrypted(s, ev, room).await }
        });

        self.status.connected();
        tracing::info!(
            homeserver = %self.settings.homeserver,
            user = %self.settings.user,
            prefix = %self.settings.prefix,
            "matrix bot listening"
        );

        let backoff = Arc::new(AtomicU64::new(1));
        let status = self.status.clone();
        client
            .sync_with_result_callback(
                SyncSettings::default().timeout(Duration::from_secs(SYNC_TIMEOUT_SECS)),
                |result| {
                    let backoff = backoff.clone();
                    let status = status.clone();
                    async move {
                        match result {
                            Ok(_) => {
                                backoff.store(1, Ordering::Relaxed);
                                status.connected();
                                Ok(LoopCtrl::Continue)
                            }
                            Err(e) => {
                                // A token the server no longer accepts needs a
                                // new login; anything else is a network blip.
                                if matches!(
                                    e.client_api_error_kind(),
                                    Some(matrix_sdk::ruma::api::client::error::ErrorKind::UnknownToken { .. })
                                ) {
                                    return Err(e);
                                }
                                tracing::warn!(error = %e, "matrix sync failed");
                                status.failed(e.to_string());
                                let wait = backoff.load(Ordering::Relaxed);
                                backoff.store((wait * 2).min(BACKOFF_MAX_SECS), Ordering::Relaxed);
                                tokio::time::sleep(Duration::from_secs(wait)).await;
                                Ok(LoopCtrl::Continue)
                            }
                        }
                    }
                },
            )
            .await?;
        Ok(())
    }

    async fn new_client(&self) -> Result<Client, AnyError> {
        Ok(Client::builder()
            .homeserver_url(&self.settings.homeserver)
            .build()
            .await?)
    }

    /// A logged-in client: the configured token, else the saved session, else
    /// a password login (whose session is then saved).
    async fn connect(&self) -> Result<Client, AnyError> {
        let s = &self.settings;
        let user_id: OwnedUserId = s.user.as_str().try_into()?;

        if !s.token.is_empty() {
            let saved = self.load_session();
            let client = self.new_client().await?;
            client
                .matrix_auth()
                .restore_session(MatrixSession {
                    meta: SessionMeta {
                        user_id: user_id.clone(),
                        device_id: saved
                            .map(|m| m.meta.device_id)
                            .unwrap_or_else(|| "YTPMACHINE".into()),
                    },
                    tokens: MatrixSessionTokens {
                        access_token: s.token.clone(),
                        refresh_token: None,
                    },
                })
                .await?;
            self.verify(&client, &user_id).await?;
            tracing::info!(user = %user_id, "matrix: using the configured access token");
            return Ok(client);
        }

        if let Some(saved) = self.load_session() {
            if saved.meta.user_id == user_id {
                let client = self.new_client().await?;
                client.matrix_auth().restore_session(saved).await?;
                match self.verify(&client, &user_id).await {
                    Ok(()) => {
                        tracing::info!(user = %user_id, "matrix: resumed the saved session");
                        return Ok(client);
                    }
                    Err(e) => {
                        tracing::info!(error = %e, "matrix: the saved session is no good, logging in again");
                        self.forget_session();
                    }
                }
            }
        }

        if s.password.is_empty() {
            return Err("no password to log in with and no usable session".into());
        }
        let client = self.new_client().await?;
        let response = client
            .matrix_auth()
            .login_username(&s.user, &s.password)
            .initial_device_display_name(&s.device_name)
            .send()
            .await?;
        self.save_session(&MatrixSession::from(&response));
        tracing::info!(user = %response.user_id, device = %response.device_id, "matrix: logged in");
        Ok(client)
    }

    async fn verify(&self, client: &Client, expected: &OwnedUserId) -> Result<(), AnyError> {
        let who = client.whoami().await?;
        if &who.user_id != expected {
            return Err(format!("the token belongs to {}, not {expected}", who.user_id).into());
        }
        Ok(())
    }

    fn load_session(&self) -> Option<MatrixSession> {
        let path = self.settings.session_file.as_deref()?;
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn save_session(&self, session: &MatrixSession) {
        let Some(path) = self.settings.session_file.as_deref() else {
            return;
        };
        if let Err(e) = write_private(path, &serde_json::to_vec(session).unwrap_or_default()) {
            tracing::warn!(error = %e, path = %path.display(), "could not save the matrix session");
        }
    }

    fn forget_session(&self) {
        if let Some(path) = self.settings.session_file.as_deref() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Write a file only its owner can read, from the first byte.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(bytes)?;
    drop(file);
    std::fs::rename(tmp, path)
}

async fn on_invite(shared: Arc<Shared>, ev: StrippedRoomMemberEvent, room: Room) {
    if ev.content.membership != MembershipState::Invite || ev.state_key != shared.own {
        return;
    }
    let room_id = room.room_id().to_string();
    let invited_by = ev.sender.to_string();
    if !shared.core.policy().matrix_allowed(&invited_by, &room_id) {
        tracing::info!(room = %room_id, invited_by, "ignoring an invite from outside the allow-list");
        return;
    }
    // Run apart from the sync loop: joining can wait on the homeserver, and
    // Synapse sometimes needs a moment before it lets an invitee in.
    tokio::spawn(async move {
        let mut wait = 1u64;
        for attempt in 1..=5 {
            match room.join().await {
                Ok(()) => {
                    tracing::info!(room = %room_id, invited_by, "joined at an invite");
                    if room.is_encrypted().await.unwrap_or(false) {
                        tracing::warn!(room = %room_id, "left: the room is encrypted, and this bot cannot read encrypted rooms");
                        let _ = room.leave().await;
                        return;
                    }
                    let hello = format!("Hello! {} help for what I can do.", shared.prefix);
                    let _ = shared.transport.send(&room_id, &hello, None).await;
                    return;
                }
                Err(e) => {
                    tracing::warn!(room = %room_id, attempt, error = %e, "could not join");
                    tokio::time::sleep(Duration::from_secs(wait)).await;
                    wait *= 2;
                }
            }
        }
        shared
            .status
            .failed(format!("could not join {room_id} (invited by {invited_by})"));
    });
}

async fn on_message(shared: Arc<Shared>, ev: OriginalSyncRoomMessageEvent, room: Room) {
    if ev.sender == shared.own {
        return;
    }
    let Some(event) = chat_event(&room, &ev) else {
        return;
    };
    tokio::spawn(async move {
        shared.core.handle(event, &shared.transport).await;
    });
}

/// Say, once per room, that the bot cannot read a word in there. An encrypted
/// room looks exactly like an idle one from the inside.
async fn on_encrypted(shared: Arc<Shared>, ev: OriginalSyncRoomEncryptedEvent, room: Room) {
    if ev.sender == shared.own {
        return;
    }
    let room_id = room.room_id().to_string();
    if !shared
        .core
        .policy()
        .matrix_allowed(ev.sender.as_str(), &room_id)
    {
        return;
    }
    if !shared.warned.lock().unwrap().insert(room_id.clone()) {
        return;
    }
    tracing::warn!(room = %room_id, "an encrypted room; this bot cannot read it");
    let _ = shared
        .transport
        .send(
            &room_id,
            "I can't read this room -- it's end-to-end encrypted, and I have no keys \
             for it. Make an unencrypted room and invite me there instead.",
            Some(ev.event_id.as_str()),
        )
        .await;
}

/// Turn a Matrix message into the core's idea of one, if it carries anything
/// the bot understands.
fn chat_event(room: &Room, ev: &OriginalSyncRoomMessageEvent) -> Option<ChatEvent> {
    if matches!(ev.content.relates_to, Some(Relation::Replacement(_))) {
        // An edit of an earlier message is not a new command.
        return None;
    }
    let midi_of = |name: &str, mime: Option<&str>, size: Option<u64>, source: &MediaSource| {
        if !is_midi(Some(name), mime) {
            return None;
        }
        if size.is_some_and(|s| s > MAX_MIDI_BYTES as u64) {
            tracing::debug!(name, "a MIDI file over the size limit; not remembered");
            return None;
        }
        match source {
            MediaSource::Plain(url) => Some(MidiRef {
                name: name.to_string(),
                key: url.to_string(),
            }),
            // Encrypted media only appears in encrypted rooms.
            _ => None,
        }
    };

    let (text, midi) = match &ev.content.msgtype {
        MessageType::Text(t) => (t.body.clone(), None),
        MessageType::Notice(n) => (n.body.clone(), None),
        MessageType::File(f) => {
            let info = f.info.as_deref();
            let midi = midi_of(
                f.filename(),
                info.and_then(|i| i.mimetype.as_deref()),
                info.and_then(|i| i.size).map(u64::from),
                &f.source,
            )?;
            (f.caption().unwrap_or("").to_string(), Some(midi))
        }
        MessageType::Audio(a) => {
            let info = a.info.as_deref();
            let midi = midi_of(
                a.filename(),
                info.and_then(|i| i.mimetype.as_deref()),
                info.and_then(|i| i.size).map(u64::from),
                &a.source,
            )?;
            (a.caption().unwrap_or("").to_string(), Some(midi))
        }
        _ => return None,
    };

    let reply_to = match &ev.content.relates_to {
        Some(Relation::Reply { in_reply_to }) => Some(in_reply_to.event_id.to_string()),
        _ => None,
    };

    Some(ChatEvent {
        platform: Platform::Matrix,
        room: room.room_id().to_string(),
        user_id: ev.sender.to_string(),
        user_name: ev.sender.localpart().to_string(),
        direct: room.direct_targets_length() > 0 || room.joined_members_count() <= 2,
        text,
        event_id: Some(ev.event_id.to_string()),
        reply_to,
        midi,
        role_ids: Vec::new(),
        guild: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1_000_000;

    fn chosen(limit: Option<u64>) -> Result<&'static str, u64> {
        match pick_upload_preset(limit) {
            PresetChoice::Preset(name) => Ok(name),
            PresetChoice::TooSmall { limit } => Err(limit),
        }
    }

    #[test]
    fn an_unknown_or_roomy_limit_keeps_matrix() {
        assert_eq!(chosen(None), Ok("matrix"));
        assert_eq!(chosen(Some(60 * MB)), Ok("matrix"));
    }

    #[test]
    fn a_smaller_limit_picks_the_largest_preset_that_fits() {
        assert_eq!(chosen(Some(40 * MB)), Ok("discord-25mb"));
        assert_eq!(chosen(Some(12 * MB)), Ok("discord-10mb"));
    }

    #[test]
    fn a_limit_below_every_preset_is_reported() {
        assert_eq!(chosen(Some(5 * MB)), Err(5 * MB));
    }
}
