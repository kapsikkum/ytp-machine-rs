//! The Matrix adapter: a [`ChatTransport`] over `matrix-sdk`, plus the loop
//! that logs in, joins invites from allowed people and feeds messages to the
//! core.
//!
//! End-to-end encrypted rooms are not supported (the `e2e-encryption` feature
//! is deliberately off, so nothing else would work either); the bot says so
//! once in a room that turns out to be encrypted instead of sitting there
//! looking idle.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::media::{MediaFormat, MediaRequestParameters};
use matrix_sdk::ruma::events::room::message::{
    MessageType, OriginalSyncRoomMessageEvent, Relation,
};
use matrix_sdk::ruma::events::room::MediaSource;
use matrix_sdk::ruma::{OwnedEventId, OwnedRoomId, RoomId};
use matrix_sdk::{
    Client, LoopCtrl, Room, RoomState, SessionMeta, SessionTokens,
};
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

/// One Matrix connection. The core talks to the room through this.
pub struct MatrixTransport {
    client: Client,
}

impl MatrixTransport {
    fn room(&self, room: &str) -> BotResult<Room> {
        let id = RoomId::parse(room)
            .map_err(|e| BotError::internal(format!("bad room id {room:?}: {e}")))?;
        self.client
            .get_room(&id)
            .ok_or_else(|| BotError::internal(format!("not in room {room}")))
    }
}

#[async_trait]
impl ChatTransport for MatrixTransport {
    fn upload_preset(&self) -> String {
        "matrix".into()
    }

    async fn send(&self, room: &str, text: &str, reply_to: Option<&str>) -> BotResult<String> {
        let room = self.room(room)?;
        let mut content = json!({ "msgtype": "m.notice", "body": text });
        if let Some(id) = reply_to {
            content["m.relates_to"] = json!({ "m.in_reply_to": { "event_id": id } });
        }
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
        let source = MediaSource::Plain(midi.key.as_str().into());
        let bytes = self
            .client
            .media()
            .get_media_content(
                &MediaRequestParameters {
                    source,
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
        let uploaded = self
            .client
            .media()
            .upload(&mime::VIDEO_MP4, data, None)
            .await
            .map_err(|e| BotError::user(format!("The upload failed ({e}).")))?;
        let mut content = json!({
            "msgtype": "m.video",
            "body": if caption.is_empty() { &file.filename } else { caption },
            "filename": file.filename,
            "url": uploaded.content_uri,
            "info": { "mimetype": "video/mp4", "size": size },
        });
        if let Some(id) = reply_to {
            content["m.relates_to"] = json!({ "m.in_reply_to": { "event_id": id } });
        }
        room.send_raw("m.room.message", content)
            .await
            .map_err(|e| BotError::user(format!("The upload failed ({e}).")))?;
        Ok(())
    }
}

/// The Matrix bot: everything needed to run and reconnect it.
pub struct MatrixBot {
    settings: MatrixSettings,
    core: Arc<BotCore>,
    status: StatusHandle,
    warned: Mutex<HashSet<String>>,
}

impl MatrixBot {
    pub fn new(settings: MatrixSettings, core: Arc<BotCore>, status: StatusHandle) -> Self {
        Self {
            settings,
            core,
            status,
            warned: Mutex::new(HashSet::new()),
        }
    }

    /// Run until the process stops, reconnecting with a growing pause. A chat
    /// network that is down is never allowed to take the video server with it.
    pub async fn run(self) {
        let mut backoff = 1u64;
        loop {
            match self.listen().await {
                Ok(()) => {
                    backoff = 1;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "matrix bot disconnected");
                    self.status.failed(e.to_string());
                    tokio::time::sleep(Duration::from_secs(backoff)).await;
                    backoff = (backoff * 2).min(BACKOFF_MAX_SECS);
                }
            }
        }
    }

    async fn listen(&self) -> Result<(), matrix_sdk::Error> {
        let client = Client::builder()
            .homeserver_url(&self.settings.homeserver)
            .build()
            .await?;
        self.login(&client).await?;
        self.status.connected();

        // Catch up without answering: anything sent while the bot was down is
        // history, and answering a morning's commands at once is worse than
        // answering none.
        let catch_up = SyncSettings::default()
            .timeout(Duration::ZERO)
            .full_state(true);
        client.sync_once(catch_up).await?;

        // Invites that arrived while it was offline are still worth taking.
        let own = client.user_id().map(|u| u.to_owned());
        for room in client.rooms() {
            if room.state() == RoomState::Invited {
                let invited_by = room
                    .invited_members()
                    .join(", ");
                if self.may_join(room.room_id().as_str(), &invited_by) {
                    self.join_and_greet(&client, &room).await;
                }
            }
        }

        let transport = Arc::new(MatrixTransport {
            client: client.clone(),
        });
        self.install_handlers(&client, transport, own);

        tracing::info!(
            homeserver = %self.settings.homeserver,
            prefix = %self.settings.prefix,
            "matrix bot listening"
        );
        let mut backoff = 1u64;
        loop {
            let settings = SyncSettings::default().timeout(Duration::from_secs(SYNC_TIMEOUT_SECS));
            match client.sync(settings).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    tracing::warn!(error = %e, "matrix sync failed");
                    self.status.failed(e.to_string());
                    tokio::time::sleep(Duration::from_secs(backoff)).await;
                    backoff = (backoff * 2).min(BACKOFF_MAX_SECS);
                }
            }
        }
    }

    async fn login(&self, client: &Client) -> Result<(), matrix_sdk::Error> {
        if !self.settings.token.is_empty() {
            let user_id = self.settings.user.as_str().try_into().map_err(|e| {
                matrix_sdk::Error::Url(matrix_sdk::ruma::IdParseError::from(e))
            })?;
            let session = matrix_sdk::authentication::matrix::MatrixSession {
                meta: SessionMeta {
                    user_id,
                    device_id: "ytp-machine".into(),
                },
                tokens: SessionTokens {
                    access_token: self.settings.token.clone(),
                    refresh_token: None,
                },
            };
            client.matrix_auth().restore_session(session).await?;
        } else {
            client
                .matrix_auth()
                .login_username(&self.settings.user, &self.settings.password)
                .initial_device_display_name(&self.settings.device_name)
                .send()
                .await?;
        }
        Ok(())
    }

    fn may_join(&self, room: &str, invited_by: &str) -> bool {
        let policy = self.core.policy();
        let allowed = policy.matrix_allowed(invited_by, room);
        if !allowed {
            tracing::info!(room, invited_by, "ignoring an invite from outside the allow-list");
        }
        allowed
    }

    async fn join_and_greet(&self, client: &Client, room: &Room) {
        match room.join().await {
            Ok(()) => {
                tracing::info!(room = %room.room_id(), "joined an invited room");
                let transport = MatrixTransport {
                    client: client.clone(),
                };
                let hello = format!("Hello! {} help for what I can do.", self.settings.prefix);
                let _ = transport.send(room.room_id().as_str(), &hello, None).await;
            }
            Err(e) => tracing::warn!(room = %room.room_id(), error = %e, "could not join"),
        }
    }

    fn install_handlers(&self, client: &Client, transport: Arc<MatrixTransport>, own: Option<matrix_sdk::ruma::OwnedUserId>) {
        let core = self.core.clone();
        let transport2 = transport.clone();
        client.add_event_handler(
            move |ev: OriginalSyncRoomMessageEvent, room: Room| {
                let core = core.clone();
                let transport = transport2.clone();
                let own = own.clone();
                async move {
                    if Some(&ev.sender) == own.as_ref() {
                        return;
                    }
                    let Some(event) = chat_event(&room, &ev) else {
                        return;
                    };
                    tokio::spawn(async move {
                        core.handle(event, transport.as_ref()).await;
                    });
                }
            },
        );

        let core = self.core.clone();
        let transport = transport.clone();
        let warned = self.warned.lock().unwrap().clone();
        let warned = Arc::new(Mutex::new(warned));
        client.add_event_handler(
            move |ev: matrix_sdk::ruma::events::room::encrypted::OriginalSyncRoomEncryptedEvent,
                  room: Room| {
                let core = core.clone();
                let transport = transport.clone();
                let warned = warned.clone();
                async move {
                    let room_id = room.room_id().to_string();
                    if !warned.lock().unwrap().insert(room_id.clone()) {
                        return;
                    }
                    if core
                        .policy()
                        .matrix_matrix_room_known(&room_id)
                    {
                        tracing::warn!(room = %room_id, "an encrypted room; this bot cannot read it");
                        let _ = transport.send(
                            &room_id,
                            "I can't read this room -- it's end-to-end encrypted, and I \
                             have no keys for it. Make an unencrypted room and invite me \
                             there instead.",
                            Some(ev.event_id.as_str()),
                        ).await;
                    }
                }
            },
        );

        let core = self.core.clone();
        let client2 = client.clone();
        let status = self.status.clone();
        client.add_event_handler(
            move |ev: matrix_sdk::ruma::events::room::member::StrippedRoomMemberEvent,
                  room: Room| {
                let core = core.clone();
                let client = client2.clone();
                let status = status.clone();
                async move {
                    if ev.content.membership != matrix_sdk::ruma::events::room::member::MembershipState::Invite {
                        return;
                    }
                    if Some(&ev.state_key) != client.user_id() {
                        return;
                    }
                    let invited_by = ev.sender.to_string();
                    let room_id = room.room_id().to_string();
                    if !core.policy().matrix_allowed(&invited_by, &room_id) {
                        tracing::info!(room = %room_id, invited_by, "ignoring an invite");
                        return;
                    }
                    match room.join().await {
                        Ok(()) => {
                            tracing::info!(room = %room_id, "joined at an invite");
                            status.connected();
                            let transport = MatrixTransport { client };
                            let hello = format!(
                                "Hello! {} help for what I can do.",
                                core.prefix()
                            );
                            let _ = transport.send(&room_id, &hello, None).await;
                        }
                        Err(e) => {
                            tracing::warn!(room = %room_id, error = %e, "could not join");
                            status.failed(format!("could not join {room_id}: {e}"));
                        }
                    }
                }
            },
        );
    }
}

/// Turn a Matrix message into the core's idea of one, if it carries anything
/// the bot understands.
fn chat_event(room: &Room, ev: &OriginalSyncRoomMessageEvent) -> Option<ChatEvent> {
    let (text, midi) = match &ev.content.msgtype {
        MessageType::Text(t) => (t.body.clone(), None),
        MessageType::Notice(n) => (n.body.clone(), None),
        MessageType::File(f) => {
            let name = f.filename();
            let mime = f.info.as_ref().and_then(|i| i.mimetype.as_deref());
            if !is_midi(Some(name), mime) {
                return None;
            }
            let source = match &f.source {
                MediaSource::Plain(url) => url.to_string(),
                _ => return None,
            };
            let caption = f.caption().unwrap_or("").to_string();
            (
                caption,
                Some(MidiRef {
                    name: name.to_string(),
                    key: source,
                }),
            )
        }
        MessageType::Audio(a) => {
            let name = a.filename();
            let mime = a.info.as_ref().and_then(|i| i.mimetype.as_deref());
            if !is_midi(Some(name), mime) {
                return None;
            }
            let source = match &a.source {
                MediaSource::Plain(url) => url.to_string(),
                _ => return None,
            };
            (
                a.caption().unwrap_or("").to_string(),
                Some(MidiRef {
                    name: name.to_string(),
                    key: source,
                }),
            )
        }
        _ => return None,
    };

    let reply_to = match &ev.content.relates_to {
        Some(Relation::Reply { in_reply_to }) => Some(in_reply_to.event_id.to_string()),
        _ => None,
    };
    let user_name = room
        .get_member_no_sync(&ev.sender)
        .ok()
        .flatten()
        .and_then(|m| m.display_name().map(str::to_string))
        .unwrap_or_else(|| ev.sender.localpart().to_string());

    Some(ChatEvent {
        platform: Platform::Matrix,
        room: room.room_id().to_string(),
        user_id: ev.sender.to_string(),
        user_name,
        direct: room.direct_targets_length() > 0 || room.joined_members_count() <= 2,
        text,
        event_id: Some(ev.event_id.to_string()),
        reply_to,
        midi,
        role_ids: Vec::new(),
        guild: None,
    })
}

// Keep the event id type in use for readers of this file; the SDK's own
// `OwnedEventId` appears in the handler signatures above via macro expansion.
#[allow(dead_code)]
fn _event_id(_: OwnedEventId, _: OwnedRoomId) {}
