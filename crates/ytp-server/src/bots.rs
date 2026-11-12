//! The chat bots on the server: [`ServerBackend`] is what `ytp-bot` runs jobs
//! through (the real job manager, MIDI library and encoder, as the bot's
//! service account), [`start`] runs the enabled bots as supervised tasks, and
//! [`status`] reports them on `GET /api/admin/bots`.
//!
//! A bot can never take the server down: each runs in its own task that is
//! restarted with a growing pause if it ends or panics, and a missing service
//! account only leaves that bot waiting.
#![cfg_attr(
    not(any(feature = "matrix", feature = "discord")),
    allow(dead_code, unused_imports)
)]

use std::collections::BTreeSet;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};
use ytp_auth::{Credential, Permission, Principal, Quota};
use ytp_bot::{
    AppIdentity, BotBackend, BotCore, BotError, BotResult, CoreConfig, EncodedFile, FinishedJob,
    JobProgress, MidiFile, PartChoice, QueueStatus, Settings, SongInfo, SongPart, StatusHandle,
    Voice,
};
use ytp_core::{Error, JobState};

use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::handlers::ytpmv::{describe_for, queue_render};
use crate::jobs::{check_options, check_text, NewJob};
use crate::AppState;

const POLL: Duration = Duration::from_millis(1000);
const JOB_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const BACKOFF_MAX_SECS: u64 = 60;
const IDENTITY_RETRY: Duration = Duration::from_secs(30);

fn user_error(e: Error) -> BotError {
    match e {
        Error::Internal(_) => BotError::internal(e.to_string()),
        other => BotError::user(other.to_string()),
    }
}

fn api_error(e: ApiError) -> BotError {
    if e.status.is_server_error() {
        BotError::internal(e.message)
    } else {
        BotError::user(e.message)
    }
}

/// What `ytp-bot` runs on: the server's own subsystems.
pub struct ServerBackend {
    st: AppState,
    /// The voice the bots speak in; the first installed corpus until someone
    /// switches it.
    voice: Mutex<Option<String>>,
    tmp: PathBuf,
}

impl ServerBackend {
    pub fn new(st: AppState) -> Self {
        let tmp = st.config.data_dir.join("bot-tmp");
        // A crash can leave encoded uploads behind; nothing here is kept.
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::create_dir_all(&tmp);
        Self {
            st,
            voice: Mutex::new(None),
            tmp,
        }
    }

    async fn load_principal(&self, username: &str) -> BotResult<Principal> {
        let user = self
            .st
            .auth
            .find_user(username)
            .await
            .map_err(user_error)?
            .ok_or_else(|| {
                BotError::internal(format!(
                    "the bot account {username:?} does not exist (create it with `user create {username}`)"
                ))
            })?;
        if user.disabled {
            return Err(BotError::internal(format!(
                "the bot account {username:?} is disabled"
            )));
        }
        let (permissions, quota) = self
            .st
            .auth
            .grants(&user.roles)
            .await
            .map_err(user_error)?;
        Ok(Principal {
            user,
            permissions,
            quota,
            credential: Credential::Token { id: 0 },
        })
    }

    async fn principal(&self, service: &AppIdentity) -> BotResult<Principal> {
        self.load_principal(&service.username).await
    }

    /// The voice to use: the one named, else the bots' current one, else the
    /// first installed.
    async fn resolve_voice(&self, requested: Option<&str>) -> BotResult<String> {
        let wanted = requested
            .map(str::to_string)
            .or_else(|| self.voice.lock().unwrap().clone());
        if let Some(v) = wanted {
            if self.st.corpora.get(&v).await.is_ok() {
                return Ok(v);
            }
            if requested.is_some() {
                return Err(BotError::user(format!(
                    "No voice called {v:?} is installed."
                )));
            }
        }
        self.st
            .corpora
            .list()
            .await
            .map_err(user_error)?
            .into_iter()
            .next()
            .map(|c| c.name)
            .ok_or_else(|| BotError::user("No corpus is installed yet."))
    }

    /// Keep a chat MIDI file in the service account's library; when it is
    /// full, make room by dropping the oldest ones nothing is rendering.
    async fn save_midi(&self, owner: &Principal, midi: &MidiFile) -> BotResult<String> {
        let id = owner.user.id;
        let save = |st: AppState, bytes: Vec<u8>, name: String| async move {
            tokio::task::spawn_blocking(move || st.ytpmv.save(id, &bytes, &name))
                .await
                .map_err(|e| Error::internal(e.to_string()))?
        };
        match save(self.st.clone(), midi.bytes.clone(), midi.name.clone()).await {
            Err(Error::Conflict(_)) => {
                let st = self.st.clone();
                tokio::task::spawn_blocking(move || {
                    let mut library = st.ytpmv.library(id);
                    // Newest first: the tail is the oldest.
                    let old = library.split_off(library.len().saturating_sub(20));
                    for v in old {
                        if let Some(mid) = v["midi_id"].as_str() {
                            if !st.jobs.midi_in_use(id, mid) {
                                let _ = st.ytpmv.delete(id, mid);
                            }
                        }
                    }
                })
                .await
                .map_err(|e| BotError::internal(e.to_string()))?;
                save(self.st.clone(), midi.bytes.clone(), midi.name.clone())
                    .await
                    .map_err(user_error)
            }
            other => other.map_err(user_error),
        }
    }
}

fn part_of(v: &Value) -> SongPart {
    let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    SongPart {
        id: s("id"),
        name: s("name"),
        role: s("role"),
        instrument: s("instrument"),
        text: v["settings"]["text"]
            .as_str()
            .filter(|t| !t.is_empty())
            .map(str::to_string),
        octave: v["settings"]["octave"].as_i64().unwrap_or(0),
    }
}

fn state_name(s: JobState) -> &'static str {
    match s {
        JobState::Queued => "queued",
        JobState::Running => "running",
        JobState::Done => "done",
        JobState::Failed => "failed",
        JobState::Cancelled => "cancelled",
    }
}

fn mime_of(ext: &str) -> &'static str {
    match ext {
        "gif" => "image/gif",
        "m4a" => "audio/mp4",
        "ogg" => "audio/ogg",
        "mkv" => "video/x-matroska",
        _ => "video/mp4",
    }
}

#[async_trait]
impl BotBackend for ServerBackend {
    async fn service_identity(&self, username: &str) -> BotResult<AppIdentity> {
        let p = self.load_principal(username).await?;
        Ok(AppIdentity {
            username: p.user.username.clone(),
            roles: p.user.roles.clone(),
            permissions: p.permissions,
            quota: p.quota,
        })
    }

    async fn grants(&self, roles: &[String]) -> BotResult<(BTreeSet<Permission>, Quota)> {
        self.st.auth.grants(roles).await.map_err(user_error)
    }

    async fn start_ytp(
        &self,
        service: &AppIdentity,
        text: &str,
        subtitles: bool,
        voice: Option<&str>,
    ) -> BotResult<String> {
        let me = self.principal(service).await?;
        let text = check_text(text).map_err(user_error)?;
        let options = json!({});
        check_options(&options).map_err(user_error)?;
        let voice = self.resolve_voice(voice).await?;
        let rec = self
            .st
            .jobs
            .submit(
                &me,
                NewJob {
                    text,
                    voice,
                    captions: subtitles,
                    options,
                },
            )
            .map_err(user_error)?;
        Ok(rec.id)
    }

    async fn ytpmv_info(
        &self,
        service: &AppIdentity,
        midi: &MidiFile,
        voice: Option<&str>,
    ) -> BotResult<SongInfo> {
        let me = self.principal(service).await?;
        let id = self.save_midi(&me, midi).await?;
        let voice = self.resolve_voice(voice).await.ok();
        let v = describe_for(&self.st, me.user.id, &id, voice.as_deref())
            .await
            .map_err(api_error)?;
        Ok(SongInfo {
            midi_id: id,
            title: v["title"].as_str().unwrap_or_default().to_string(),
            description: v["description"].as_str().unwrap_or_default().to_string(),
            parts: v["parts"]
                .as_array()
                .map(|a| a.iter().map(part_of).collect())
                .unwrap_or_default(),
        })
    }

    async fn start_ytpmv(
        &self,
        service: &AppIdentity,
        midi: &MidiFile,
        parts: &[PartChoice],
        options: &Value,
        voice: Option<&str>,
    ) -> BotResult<String> {
        let me = self.principal(service).await?;
        let id = self.save_midi(&me, midi).await?;
        let voice = self.resolve_voice(voice).await.ok();
        let parts: Vec<Value> = parts
            .iter()
            .map(|p| {
                let mut v = p.settings.clone();
                if let Value::Object(m) = &mut v {
                    m.insert("id".into(), json!(p.id));
                }
                v
            })
            .collect();
        let body = json!({
            "midi_id": id,
            "parts": parts,
            "options": options,
            "voice": voice,
        });
        let queued = queue_render(&self.st, &me, &body)
            .await
            .map_err(api_error)?;
        queued["job_id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| BotError::internal("the render was queued without a job id"))
    }

    async fn list_voices(&self) -> BotResult<Vec<Voice>> {
        let corpora = self.st.corpora.list().await.map_err(user_error)?;
        let current = self.voice.lock().unwrap().clone();
        let active = current
            .filter(|c| corpora.iter().any(|x| &x.name == c))
            .or_else(|| corpora.first().map(|c| c.name.clone()));
        Ok(corpora
            .into_iter()
            .map(|c| Voice {
                active: active.as_deref() == Some(c.name.as_str()),
                words: u64::try_from(c.words).ok(),
                name: c.name,
            })
            .collect())
    }

    async fn switch_voice(&self, service: &AppIdentity, name: &str) -> BotResult<Voice> {
        let name = name.trim();
        let corpus = self
            .st
            .corpora
            .get(name)
            .await
            .map_err(|_| BotError::user(format!("No voice called {name:?} is installed.")))?;
        *self.voice.lock().unwrap() = Some(corpus.name.clone());
        tracing::info!(voice = %corpus.name, by = %service.username, "the bots' voice was switched");
        Ok(Voice {
            active: true,
            words: u64::try_from(corpus.words).ok(),
            name: corpus.name,
        })
    }

    async fn queue_status(&self) -> BotResult<QueueStatus> {
        let (running, queued) = self.st.jobs.counts();
        Ok(QueueStatus { running, queued })
    }

    async fn wait_job(
        &self,
        job_id: &str,
        on_progress: &(dyn for<'a> Fn(&'a JobProgress) + Send + Sync),
    ) -> BotResult<FinishedJob> {
        let started = std::time::Instant::now();
        let mut last: Option<(JobState, i32, Option<String>)> = None;
        loop {
            let Some(rec) = self.st.jobs.get(job_id) else {
                return Err(BotError::user("That job has gone."));
            };
            let key = (
                rec.state,
                (rec.progress * 100.0) as i32,
                rec.message.clone(),
            );
            if last.as_ref() != Some(&key) {
                on_progress(&JobProgress {
                    state: state_name(rec.state).into(),
                    progress: rec.progress,
                    message: rec.message.clone(),
                });
                last = Some(key);
            }
            match rec.state {
                JobState::Done => {
                    return Ok(FinishedJob {
                        job_id: rec.id,
                        report: rec.report.unwrap_or(Value::Null),
                        duration: rec.duration,
                    })
                }
                JobState::Failed => {
                    return Err(BotError::user(
                        rec.error.unwrap_or_else(|| "It went wrong.".into()),
                    ))
                }
                JobState::Cancelled => return Err(BotError::user("That job was cancelled.")),
                JobState::Queued | JobState::Running => {}
            }
            if started.elapsed() > JOB_TIMEOUT {
                return Err(BotError::user("That took far too long, so I gave up waiting."));
            }
            tokio::time::sleep(POLL).await;
        }
    }

    async fn encode_result(
        &self,
        service: &AppIdentity,
        job_id: &str,
        preset: &str,
    ) -> BotResult<EncodedFile> {
        let me = self.principal(service).await?;
        let rec = self
            .st
            .jobs
            .get(job_id)
            .ok_or_else(|| BotError::user("That job has gone."))?;
        if rec.state != JobState::Done {
            return Err(BotError::user("That job has no finished video."));
        }
        let input = crate::encode::source_file(&self.st, &rec)
            .ok_or_else(|| BotError::internal("the render has no video file to encode"))?;
        let preset = self
            .st
            .encoder
            .presets()
            .get(preset)
            .cloned()
            .ok_or_else(|| BotError::internal(format!("no encode preset named {preset:?}")))?;
        let ext = preset.extension().to_string();
        let filename = format!("ytp-{job_id}.{ext}");
        let out = self.tmp.join(format!("{job_id}-{}.{ext}", preset.name));

        let st = self.st.clone();
        let overrides = ytp_encode::Overrides {
            max_width: 0,
            max_height: me.quota.max_height,
        };
        let (input2, out2) = (input, out.clone());
        let encoded = tokio::task::spawn_blocking(move || {
            st.encoder
                .encode(&input2, &preset, overrides, &out2, &|_| {}, &|| false)
        })
        .await
        .map_err(|e| BotError::internal(format!("the encoder crashed: {e}")))?;
        match encoded {
            Ok(o) => Ok(EncodedFile {
                path: out,
                filename,
                mime: mime_of(&ext).into(),
                bytes: o.bytes,
            }),
            Err(e) => {
                let _ = std::fs::remove_file(&out);
                Err(BotError::internal(format!("the encode failed: {e}")))
            }
        }
    }

    async fn release(&self, file: &EncodedFile) {
        let _ = tokio::fs::remove_file(&file.path).await;
    }
}

// â”€â”€ running the bots â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

struct Entry {
    platform: &'static str,
    enabled: bool,
    status: Option<Arc<StatusHandle>>,
    detail: Value,
}

/// What `GET /api/admin/bots` reports.
#[derive(Default)]
pub struct BotRegistry {
    service_user: Mutex<Option<String>>,
    entries: Mutex<Vec<Entry>>,
}

impl BotRegistry {
    fn register(&self, entry: Entry) {
        self.entries.lock().unwrap().push(entry);
    }

    pub fn report(&self) -> Value {
        let bots: Vec<Value> = self
            .entries
            .lock()
            .unwrap()
            .iter()
            .map(|e| {
                let s = e.status.as_ref().map(|s| s.get());
                json!({
                    "platform": e.platform,
                    "enabled": e.enabled,
                    "connected": s.as_ref().is_some_and(|s| s.connected),
                    "last_error": s.as_ref().and_then(|s| s.last_error.clone()),
                    "since": s.as_ref().and_then(|s| s.since.clone()),
                    "detail": e.detail,
                })
            })
            .collect();
        json!({
            "service_user": *self.service_user.lock().unwrap(),
            "bots": bots,
        })
    }
}

/// `GET /api/admin/bots`: the state of each chat bot. Never includes tokens
/// or passwords.
pub async fn status(State(st): State<AppState>, _me: CurrentUser) -> ApiResult<Json<Value>> {
    Ok(Json(st.bots.report()))
}

/// Run `make()` for as long as the server lives: when it returns or panics,
/// say so and run it again after a growing pause.
async fn supervise<F, Fut>(name: &'static str, status: Arc<StatusHandle>, make: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let mut backoff = 1u64;
    loop {
        let started = std::time::Instant::now();
        match tokio::spawn(make()).await {
            Ok(()) => tracing::warn!(bot = name, "the bot task ended"),
            Err(e) => {
                tracing::error!(bot = name, error = %e, "the bot task crashed");
                status.failed(format!("crashed: {e}"));
            }
        }
        if started.elapsed() > Duration::from_secs(120) {
            backoff = 1;
        }
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(BACKOFF_MAX_SECS);
    }
}

/// Wait for the bot's service account to exist and be usable.
async fn service_identity(
    backend: &ServerBackend,
    username: &str,
    status: &StatusHandle,
) -> AppIdentity {
    loop {
        match backend.service_identity(username).await {
            Ok(id) => return id,
            Err(e) => {
                tracing::warn!(error = %e, "chat bot waiting for its service account");
                status.failed(e.to_string());
                tokio::time::sleep(IDENTITY_RETRY).await;
            }
        }
    }
}

/// Start the bots that the environment enables. Returns at once; the bots run
/// as background tasks for the life of the process.
pub fn start(st: &AppState) {
    let mut settings = Settings::from_env();
    if let Some(m) = settings.matrix.as_mut() {
        m.session_file = Some(st.config.data_dir.join("matrix").join("session.json"));
    }
    *st.bots.service_user.lock().unwrap() = Some(settings.service_user.clone());

    if !settings.any_enabled() {
        for platform in ["matrix", "discord"] {
            st.bots.register(Entry {
                platform,
                enabled: false,
                status: None,
                detail: json!({}),
            });
        }
        tracing::info!("no chat bots configured (see the README's \"Chat bots\" section)");
        return;
    }

    let run = Run {
        backend: Arc::new(ServerBackend::new(st.clone())),
        policy: settings.policy(),
        service_user: settings.service_user.clone(),
        mv_max: settings.mv_max_seconds as f64,
    };
    start_matrix(st, &settings, &run);
    start_discord(st, &settings, &run);
}

/// What the platform tasks share.
#[derive(Clone)]
struct Run {
    backend: Arc<ServerBackend>,
    policy: ytp_bot::BotPolicy,
    service_user: String,
    mv_max: f64,
}

fn disabled(st: &AppState, platform: &'static str, configured: bool) {
    if configured {
        tracing::warn!(
            platform,
            "this bot is configured but this build was compiled without it"
        );
    }
    st.bots.register(Entry {
        platform,
        enabled: false,
        status: None,
        detail: json!({}),
    });
}

#[cfg(feature = "matrix")]
fn start_matrix(st: &AppState, settings: &Settings, run: &Run) {
    let Some(m) = settings.matrix.clone() else {
        return disabled(st, "matrix", false);
    };
    let status = Arc::new(StatusHandle::new());
    st.bots.register(Entry {
        platform: "matrix",
        enabled: true,
        status: Some(status.clone()),
        detail: json!({
            "homeserver": m.homeserver,
            "user": m.user,
            "login": if m.token.is_empty() { "password" } else { "token" },
            "prefix": m.prefix,
            "rooms": m.rooms.len(),
            "users": m.users.len(),
        }),
    });
    let run = run.clone();
    tokio::spawn(async move {
        let service = service_identity(&run.backend, &run.service_user, &status).await;
        let supervised = status.clone();
        supervise("matrix", supervised, move || {
            let core = Arc::new(BotCore::new(
                run.backend.clone(),
                run.policy.clone(),
                service.clone(),
                CoreConfig {
                    prefix: m.prefix.clone(),
                    mv_max_seconds: run.mv_max,
                    ..CoreConfig::default()
                },
            ));
            ytp_bot::matrix::MatrixBot::new(m.clone(), core, status.clone()).run()
        })
        .await;
    });
}

#[cfg(not(feature = "matrix"))]
fn start_matrix(st: &AppState, settings: &Settings, _run: &Run) {
    disabled(st, "matrix", settings.matrix.is_some());
}

#[cfg(feature = "discord")]
fn start_discord(st: &AppState, settings: &Settings, run: &Run) {
    let Some(d) = settings.discord.clone() else {
        return disabled(st, "discord", false);
    };
    let status = Arc::new(StatusHandle::new());
    st.bots.register(Entry {
        platform: "discord",
        enabled: true,
        status: Some(status.clone()),
        detail: json!({
            "prefix": d.prefix,
            "prefix_commands": d.prefix_commands,
            "message_content_intent": d.message_content,
            "guilds": d.guilds.len(),
            "channels": d.channels.len(),
            "users": d.users.len(),
            "role_mappings": d.roles.len(),
        }),
    });
    let run = run.clone();
    tokio::spawn(async move {
        let service = service_identity(&run.backend, &run.service_user, &status).await;
        let supervised = status.clone();
        supervise("discord", supervised, move || {
            let core = Arc::new(BotCore::new(
                run.backend.clone(),
                run.policy.clone(),
                service.clone(),
                CoreConfig {
                    prefix: d.prefix.clone(),
                    mv_max_seconds: run.mv_max,
                    ..CoreConfig::default()
                },
            ));
            ytp_bot::discord::DiscordBot::new(d.clone(), core, status.clone()).run()
        })
        .await;
    });
}

#[cfg(not(feature = "discord"))]
fn start_discord(st: &AppState, settings: &Settings, _run: &Run) {
    disabled(st, "discord", settings.discord.is_some());
}