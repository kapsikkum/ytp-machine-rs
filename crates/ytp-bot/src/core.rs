//! The platform-agnostic bot: parse a message, decide whether this person may
//! ask for that, run the server job while editing a status message, then hand
//! the encoded video back to the chat network.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};
use ytp_auth::{Permission, Throttle, Throttled};

use crate::backend::{
    AppIdentity, BotBackend, BotResult, FinishedJob, JobProgress, MidiFile, MidiRef, PartChoice,
    SongInfo,
};
use crate::commands::{
    help_text, match_part, parse, Command, CommandName, PartInfo, DEFAULT_PREFIX,
};
use crate::error::BotError;
use crate::policy::BotPolicy;
use crate::transport::{ChatEvent, ChatTransport, Platform};

const MAX_MIDI_EVENTS: usize = 500;
const MAX_ROOMS: usize = 256;
const MAX_IN_FLIGHT: usize = 32;

/// Limits that are not about permissions.
#[derive(Debug, Clone)]
pub struct CoreConfig {
    pub prefix: String,
    /// Longest song a chat command may make, in seconds.
    pub mv_max_seconds: f64,
    pub max_in_flight: usize,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            prefix: DEFAULT_PREFIX.to_string(),
            mv_max_seconds: 120.0,
            max_in_flight: MAX_IN_FLIGHT,
        }
    }
}

#[derive(Default)]
struct MidiMemory {
    by_event: HashMap<String, MidiRef>,
    by_room: HashMap<String, MidiRef>,
    order: VecDeque<String>,
}

impl MidiMemory {
    fn remember(&mut self, room: &str, event_id: &str, midi: MidiRef) {
        self.by_event.insert(event_id.to_string(), midi.clone());
        self.order.push_back(event_id.to_string());
        self.by_room.insert(room.to_string(), midi);
        while self.order.len() > MAX_MIDI_EVENTS {
            if let Some(old) = self.order.pop_front() {
                self.by_event.remove(&old);
            }
        }
        if self.by_room.len() > MAX_ROOMS {
            if let Some(old) = self.by_room.keys().next().cloned() {
                self.by_room.remove(&old);
            }
        }
    }

    fn get(&self, room: &str, reply_to: Option<&str>) -> Option<MidiRef> {
        if let Some(id) = reply_to {
            if let Some(m) = self.by_event.get(id) {
                return Some(m.clone());
            }
        }
        self.by_room.get(room).cloned()
    }
}

pub struct BotCore {
    backend: Arc<dyn BotBackend>,
    policy: BotPolicy,
    service: AppIdentity,
    config: CoreConfig,
    throttle: Throttle,
    midi: Mutex<MidiMemory>,
    busy: Mutex<HashSet<String>>,
    voice: Mutex<Option<String>>,
}

impl BotCore {
    pub fn new(
        backend: Arc<dyn BotBackend>,
        policy: BotPolicy,
        service: AppIdentity,
        config: CoreConfig,
    ) -> Self {
        Self {
            backend,
            policy,
            service,
            config,
            throttle: Throttle::default(),
            midi: Mutex::new(MidiMemory::default()),
            busy: Mutex::new(HashSet::new()),
            voice: Mutex::new(None),
        }
    }

    pub fn prefix(&self) -> &str {
        &self.config.prefix
    }

    pub fn policy(&self) -> &BotPolicy {
        &self.policy
    }

    /// The voices the bot could switch to, for autocomplete.
    pub async fn voice_names(&self) -> BotResult<Vec<String>> {
        Ok(self
            .backend
            .list_voices()
            .await?
            .into_iter()
            .map(|v| v.name)
            .collect())
    }

    async fn current_voice(&self) -> Option<String> {
        if let Some(v) = self.voice.lock().unwrap().clone() {
            return Some(v);
        }
        let voices = self.backend.list_voices().await.ok()?;
        let name = voices
            .iter()
            .find(|v| v.active)
            .or_else(|| voices.first())
            .map(|v| v.name.clone());
        if let Some(n) = &name {
            *self.voice.lock().unwrap() = Some(n.clone());
        }
        name
    }

    /// Handle one chat message. Errors that the person should see are sent as
    /// replies; nothing here is allowed to bring the adapter down.
    pub async fn handle(&self, event: ChatEvent, transport: &dyn ChatTransport) {
        // Nothing from outside the allow-list is looked at, not even a MIDI
        // file someone else might later ask to be used.
        let allowed = match event.platform {
            Platform::Matrix => self.policy.matrix_allowed(&event.user_id, &event.room),
            Platform::Discord => self.policy.discord_allowed(
                &event.user_id,
                event.guild.as_deref(),
                &event.room,
            ),
        };
        if !allowed {
            tracing::debug!(user = %event.user_id, room = %event.room, "ignoring a message from outside the allow-list");
            return;
        }

        if let (Some(event_id), Some(midi)) = (event.event_id.as_deref(), event.midi.as_ref()) {
            self.midi
                .lock()
                .unwrap()
                .remember(&event.room, event_id, midi.clone());
        }

        let Some(cmd) = parse(&event.text, &self.config.prefix, event.direct) else {
            return;
        };

        match cmd.name {
            CommandName::Help => {
                let extra = if cmd.errors.is_empty() {
                    String::new()
                } else {
                    format!("\n{}", cmd.errors.join("; "))
                };
                self.reply(
                    transport,
                    &event,
                    &format!("{}{extra}", help_text(&self.config.prefix)),
                )
                .await;
                return;
            }
            CommandName::Unknown => {
                self.reply(
                    transport,
                    &event,
                    &format!(
                        "I don't know {:?} — try {} help",
                        cmd.text, self.config.prefix
                    ),
                )
                .await;
                return;
            }
            _ => {}
        }

        let roles = match event.platform {
            Platform::Matrix => self.policy.matrix_roles(),
            Platform::Discord => self.policy.discord_roles(&event.role_ids),
        };
        let (permissions, quota) = match self.backend.grants(&roles).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "could not resolve a chat user's roles");
                self.reply(
                    transport,
                    &event,
                    "❌ The bot isn't set up right (its roles could not be read). It's in the log.",
                )
                .await;
                return;
            }
        };

        if let Some(needed) = self.needed_permissions(&cmd) {
            for p in needed {
                if !permissions.contains(&p) {
                    self.reply(
                        transport,
                        &event,
                        &format!("❌ You don't have permission to do that (missing {p})."),
                    )
                    .await;
                    return;
                }
            }
        }

        let heavy = matches!(
            cmd.name,
            CommandName::Say | CommandName::Mv | CommandName::Info
        );
        let user_key = format!("chat:{}", event.user_id);
        if heavy {
            // Decide under the lock, but reply outside it: the lock must not be
            // held across the awaits below.
            let (over_limit, already_running) = {
                let mut busy = self.busy.lock().unwrap();
                if busy.len() >= self.config.max_in_flight {
                    (true, false)
                } else {
                    (false, !busy.insert(user_key.clone()))
                }
            };
            if over_limit {
                self.reply(
                    transport,
                    &event,
                    "Too many requests are in flight — try again shortly.",
                )
                .await;
                return;
            }
            if already_running {
                self.reply(
                    transport,
                    &event,
                    "Still working on your last one — hang on.",
                )
                .await;
                return;
            }
            // `mv info` only reads and uploads a MIDI file, which the server
            // does not count as a request either.
            if cmd.name != CommandName::Info {
                match self.throttle.take_key(&user_key, quota.requests_per_minute) {
                    Ok(()) => {}
                    Err(Throttled::Off) => {
                        self.busy.lock().unwrap().remove(&user_key);
                        self.reply(
                            transport,
                            &event,
                            "❌ Your role may not make these requests.",
                        )
                        .await;
                        return;
                    }
                    Err(Throttled::Wait(secs)) => {
                        self.busy.lock().unwrap().remove(&user_key);
                        self.reply(transport, &event, &format!("❌ Give it {secs}s."))
                            .await;
                        return;
                    }
                }
            }
            if let Some(id) = event.event_id.as_deref() {
                transport.react(&event.room, id, "👀").await;
            }
        }

        let result = match cmd.name {
            CommandName::Say => self.cmd_say(&event, &cmd, transport).await,
            CommandName::Mv => self.cmd_mv(&event, &cmd, transport).await,
            CommandName::Info => self.cmd_info(&event, transport).await,
            CommandName::Voices => self.cmd_voices(&event, transport).await,
            CommandName::Voice => self.cmd_voice(&event, &cmd, transport).await,
            CommandName::Queue => self.cmd_queue(&event, transport).await,
            CommandName::Help | CommandName::Unknown => Ok(()),
        };

        if heavy {
            self.busy.lock().unwrap().remove(&user_key);
        }
        if let Err(e) = result {
            match e {
                BotError::User(msg) => self.reply(transport, &event, &format!("❌ {msg}")).await,
                BotError::Internal(msg) => {
                    tracing::warn!(error = %msg, command = cmd.name.as_str(), "a bot command failed");
                    self.reply(transport, &event, "❌ Something broke. It's in the log.")
                        .await;
                }
            }
        }
    }

    fn needed_permissions(&self, cmd: &Command) -> Option<Vec<Permission>> {
        match cmd.name {
            CommandName::Say => Some(vec![Permission::GenerateYtp]),
            CommandName::Mv => Some(vec![Permission::GenerateYtpmv, Permission::ExportRender]),
            CommandName::Info => Some(vec![Permission::GenerateYtpmv]),
            CommandName::Voices => Some(vec![Permission::CorpusRead]),
            CommandName::Voice => Some(vec![Permission::CorpusEdit]),
            _ => None,
        }
    }

    async fn reply(&self, transport: &dyn ChatTransport, event: &ChatEvent, text: &str) {
        if let Err(e) = transport
            .send(&event.room, text, event.event_id.as_deref())
            .await
        {
            tracing::warn!(error = %e, room = %event.room, "could not send a chat reply");
        }
    }

    async fn midi_for(
        &self,
        event: &ChatEvent,
        transport: &dyn ChatTransport,
    ) -> BotResult<MidiFile> {
        let found = self
            .midi
            .lock()
            .unwrap()
            .get(&event.room, event.reply_to.as_deref());
        let Some(midi) = found else {
            return Err(BotError::user(
                "Post a MIDI file here first (or reply to one), then ask again.",
            ));
        };
        transport.fetch_midi(&midi).await
    }

    async fn cmd_say(
        &self,
        event: &ChatEvent,
        cmd: &Command,
        transport: &dyn ChatTransport,
    ) -> BotResult<()> {
        let status = transport
            .send(
                &event.room,
                &format!("⏳ saying: {}", truncate(&cmd.text, 80)),
                event.event_id.as_deref(),
            )
            .await
            .ok();
        let voice = self.current_voice().await;
        let job = self
            .backend
            .start_ytp(&self.service, &cmd.text, cmd.subtitles, voice.as_deref())
            .await?;
        let done = self
            .wait(&job, &event.room, status.as_deref(), "saying it", transport)
            .await?;
        let missing = done
            .report
            .get("missing")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let note = if missing.is_empty() {
            String::new()
        } else {
            format!(" (couldn't say: {missing})")
        };
        self.upload(&done.job_id, event, &format!("{}{note}", cmd.text), transport)
            .await?;
        if let Some(id) = &status {
            let _ = transport
                .edit(&event.room, id, &format!("✅ said it{note}"))
                .await;
        }
        Ok(())
    }

    async fn cmd_mv(
        &self,
        event: &ChatEvent,
        cmd: &Command,
        transport: &dyn ChatTransport,
    ) -> BotResult<()> {
        if !cmd.errors.is_empty() {
            self.reply(
                transport,
                event,
                &format!("Didn't understand: {}", cmd.errors.join("; ")),
            )
            .await;
            return Ok(());
        }
        let midi = self.midi_for(event, transport).await?;
        let voice = self.current_voice().await;
        let song = self
            .backend
            .ytpmv_info(&self.service, &midi, voice.as_deref())
            .await?;
        let parts_info: Vec<PartInfo> = song
            .parts
            .iter()
            .map(|p| PartInfo {
                id: p.id.clone(),
                name: p.name.clone(),
                role: p.role.clone(),
                instrument: p.instrument.clone(),
            })
            .collect();
        let mut parts = Vec::new();
        let mut unknown = Vec::new();
        for (name, settings) in &cmd.parts {
            match match_part(name, &parts_info) {
                Some(p) => parts.push(PartChoice {
                    id: p.id.clone(),
                    settings: Value::Object(settings.clone()),
                }),
                None => unknown.push(name.clone()),
            }
        }
        if !unknown.is_empty() {
            let mut roles: Vec<&str> = song
                .parts
                .iter()
                .map(|p| p.role.rsplit(':').next().unwrap_or(&p.role))
                .collect();
            roles.sort_unstable();
            roles.dedup();
            self.reply(
                transport,
                event,
                &format!(
                    "This song has no part called {}. Its parts: {}.",
                    unknown.join(", "),
                    roles.join(", ")
                ),
            )
            .await;
            return Ok(());
        }

        let mut options: Map<String, Value> = cmd.options.clone();
        let asked = options
            .get("max_seconds")
            .and_then(Value::as_f64)
            .unwrap_or(self.config.mv_max_seconds);
        options.insert(
            "max_seconds".into(),
            json!(asked.min(self.config.mv_max_seconds)),
        );
        let options = Value::Object(options);
        let title = if song.title.is_empty() {
            "the song".to_string()
        } else {
            song.title.clone()
        };
        let status = transport
            .send(
                &event.room,
                &format!("⏳ singing {title}: {}", song.description),
                event.event_id.as_deref(),
            )
            .await
            .ok();
        let job = self
            .backend
            .start_ytpmv(&self.service, &midi, &parts, &options, voice.as_deref())
            .await?;
        let done = self
            .wait(&job, &event.room, status.as_deref(), &format!("singing {title}"), transport)
            .await?;

        let words = done
            .report
            .get("parts")
            .and_then(Value::as_array)
            .map(|parts| {
                let mut out = Vec::new();
                for p in parts {
                    let text = p.get("text").and_then(Value::as_str).unwrap_or("");
                    let mute = p.get("mute").and_then(Value::as_bool).unwrap_or(false);
                    if text.is_empty() || mute {
                        continue;
                    }
                    let name = p
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("part")
                        .split(" (")
                        .next()
                        .unwrap_or("part");
                    out.push(format!("{name}: {text}"));
                }
                out.join(", ")
            })
            .unwrap_or_default();
        let caption = if words.is_empty() {
            title.clone()
        } else {
            format!("{title} — {words}")
        };
        self.upload(&done.job_id, event, &caption, transport).await?;

        let problems = done
            .report
            .get("problems")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|p| {
                        let name = p.get("name").and_then(Value::as_str).unwrap_or("a part");
                        let err = p.get("error").and_then(Value::as_str).unwrap_or("");
                        (!err.is_empty()).then(|| format!("{name}: {err}"))
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .unwrap_or_default();
        if let Some(id) = &status {
            let text = if problems.is_empty() {
                "✅ done".to_string()
            } else {
                format!("✅ done (left out {problems})")
            };
            let _ = transport.edit(&event.room, id, &text).await;
        }
        Ok(())
    }

    async fn cmd_info(&self, event: &ChatEvent, transport: &dyn ChatTransport) -> BotResult<()> {
        let midi = self.midi_for(event, transport).await?;
        let voice = self.current_voice().await;
        let song = self
            .backend
            .ytpmv_info(&self.service, &midi, voice.as_deref())
            .await?;
        self.reply(transport, event, &info_text(&song, &self.config.prefix))
            .await;
        Ok(())
    }

    async fn cmd_voices(&self, event: &ChatEvent, transport: &dyn ChatTransport) -> BotResult<()> {
        let voices = self.backend.list_voices().await?;
        if let Some(active) = voices.iter().find(|v| v.active) {
            *self.voice.lock().unwrap() = Some(active.name.clone());
        }
        let mut lines = vec!["Voices:".to_string()];
        if voices.is_empty() {
            lines.push("  (none installed)".into());
        }
        for v in &voices {
            lines.push(format!(
                "{} {} — {}",
                if v.active { "▶" } else { " " },
                v.name,
                v.words
                    .map(|w| format!("{w} words"))
                    .unwrap_or_else(|| "? words".into())
            ));
        }
        lines.push(format!(
            "\n{} voice <name> to switch",
            self.config.prefix
        ));
        self.reply(transport, event, &lines.join("\n")).await;
        Ok(())
    }

    async fn cmd_voice(
        &self,
        event: &ChatEvent,
        cmd: &Command,
        transport: &dyn ChatTransport,
    ) -> BotResult<()> {
        let name = cmd.text.trim();
        let voice = self.backend.switch_voice(&self.service, name).await?;
        *self.voice.lock().unwrap() = Some(voice.name.clone());
        self.reply(
            transport,
            event,
            &format!(
                "Now speaking as {}{}.",
                voice.name,
                voice
                    .words
                    .map(|w| format!(" ({w} words)"))
                    .unwrap_or_default()
            ),
        )
        .await;
        Ok(())
    }

    async fn cmd_queue(&self, event: &ChatEvent, transport: &dyn ChatTransport) -> BotResult<()> {
        let q = self.backend.queue_status().await?;
        self.reply(
            transport,
            event,
            &format!("{} running, {} waiting.", q.running, q.queued),
        )
        .await;
        Ok(())
    }

    async fn upload(
        &self,
        job_id: &str,
        event: &ChatEvent,
        caption: &str,
        transport: &dyn ChatTransport,
    ) -> BotResult<()> {
        let preset = transport.upload_preset();
        let file = self
            .backend
            .encode_result(&self.service, job_id, &preset)
            .await?;
        let sent = transport
            .send_video(&event.room, &file, caption, event.event_id.as_deref())
            .await;
        self.backend.release(&file).await;
        sent
    }

    /// Poll a job while keeping one chat message up to date.
    async fn wait(
        &self,
        job_id: &str,
        room: &str,
        status: Option<&str>,
        what: &str,
        transport: &dyn ChatTransport,
    ) -> BotResult<FinishedJob> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<JobProgress>();
        let on_progress = move |p: &JobProgress| {
            let _ = tx.send(p.clone());
        };
        let fut = self.backend.wait_job(job_id, &on_progress);
        tokio::pin!(fut);
        let mut last = String::new();
        let outcome = loop {
            tokio::select! {
                r = &mut fut => break r,
                Some(p) = rx.recv() => {
                    update_status(transport, room, status, what, &p, &mut last).await;
                }
            }
        };
        while let Ok(p) = rx.try_recv() {
            update_status(transport, room, status, what, &p, &mut last).await;
        }
        outcome
    }
}

async fn update_status(
    transport: &dyn ChatTransport,
    room: &str,
    status: Option<&str>,
    what: &str,
    p: &JobProgress,
    last: &mut String,
) {
    let Some(id) = status else { return };
    let text = progress_text(what, p);
    if text != *last {
        *last = text.clone();
        let _ = transport.edit(room, id, &text).await;
    }
}

fn progress_text(what: &str, p: &JobProgress) -> String {
    if p.state == "queued" {
        return format!("⏳ {what}: queued");
    }
    let stage = p
        .message
        .as_deref()
        .filter(|m| !m.is_empty())
        .unwrap_or(&p.state);
    if p.progress > 0.0 && p.progress < 1.0 {
        format!("⏳ {what}: {stage} {}%", (p.progress * 100.0).round() as i64)
    } else {
        format!("⏳ {what}: {stage}")
    }
}

fn info_text(song: &SongInfo, prefix: &str) -> String {
    let title = if song.title.is_empty() {
        "that song"
    } else {
        &song.title
    };
    let mut lines = vec![format!("🎼 {title}: {}", song.description)];
    for p in &song.parts {
        let role = p.role.rsplit(':').next().unwrap_or(&p.role);
        let octave = if p.octave != 0 {
            format!(" (octave {:+})", p.octave)
        } else {
            String::new()
        };
        lines.push(format!(
            "  • {:<8} {:<22} → {}{octave}",
            role,
            p.instrument,
            p.text.clone().unwrap_or_else(|| "—".into())
        ));
    }
    lines.push(format!(
        "Change one with e.g. {prefix} mv lead=yeah kick=boom"
    ));
    lines.join("\n")
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
