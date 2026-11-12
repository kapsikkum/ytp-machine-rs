//! The core flow, exercised without a chat network or a video server.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ytp_auth::{Permission, Quota};
use ytp_bot::{
    AppIdentity, BotBackend, BotCore, BotError, BotPolicy, BotResult, ChatEvent, ChatTransport,
    CoreConfig, EncodedFile, FinishedJob, JobProgress, MatrixAccess, MidiFile, MidiRef, PartChoice,
    Platform, QueueStatus, SongInfo, SongPart, Voice,
};

/// What the fake backend records for a finished mv: the parts it renders,
/// the song options it was asked for, and the voice it switched to.
type MvRecording = (Vec<PartChoice>, serde_json::Value, Option<String>);

fn quota(per_minute: u32) -> Quota {
    Quota {
        max_concurrent_jobs: 1,
        max_render_seconds: 60,
        max_height: 720,
        requests_per_minute: per_minute,
    }
}

struct FakeBackend {
    permissions: BTreeSet<Permission>,
    quota: Quota,
    voices: Mutex<Vec<Voice>>,
    ytp: Mutex<Vec<(String, bool, Option<String>)>>,
    ytpmv: Mutex<Vec<MvRecording>>,
    switched: Mutex<Option<String>>,
}

impl Default for FakeBackend {
    fn default() -> Self {
        Self {
            permissions: BTreeSet::new(),
            quota: Quota::ZERO,
            voices: Mutex::new(Vec::new()),
            ytp: Mutex::new(Vec::new()),
            ytpmv: Mutex::new(Vec::new()),
            switched: Mutex::new(None),
        }
    }
}

impl FakeBackend {
    fn new(permissions: &[Permission], per_minute: u32) -> Arc<Self> {
        let mut set = BTreeSet::new();
        set.extend(permissions.iter().copied());
        Arc::new(Self {
            permissions: set,
            quota: quota(per_minute),
            voices: Mutex::new(vec![Voice {
                name: "dank-pods".into(),
                words: Some(42),
                active: true,
            }]),
            ..Default::default()
        })
    }
}

#[async_trait]
impl BotBackend for FakeBackend {
    async fn service_identity(&self, username: &str) -> BotResult<AppIdentity> {
        Ok(AppIdentity {
            username: username.into(),
            roles: vec!["admin".into()],
            permissions: self.permissions.clone(),
            quota: self.quota,
        })
    }

    async fn grants(&self, _roles: &[String]) -> BotResult<(BTreeSet<Permission>, Quota)> {
        Ok((self.permissions.clone(), self.quota))
    }

    async fn start_ytp(
        &self,
        _service: &AppIdentity,
        text: &str,
        subtitles: bool,
        voice: Option<&str>,
    ) -> BotResult<String> {
        self.ytp
            .lock()
            .unwrap()
            .push((text.into(), subtitles, voice.map(str::to_string)));
        Ok("job-1".into())
    }

    async fn ytpmv_info(
        &self,
        _service: &AppIdentity,
        _midi: &MidiFile,
        _voice: Option<&str>,
    ) -> BotResult<SongInfo> {
        Ok(SongInfo {
            midi_id: "song".into(),
            title: "Never Gonna Give You Up".into(),
            description: "a song".into(),
            parts: vec![
                SongPart {
                    id: "lead".into(),
                    name: "Lead".into(),
                    role: "chord:lead".into(),
                    instrument: "piano".into(),
                    text: Some("never".into()),
                    octave: 0,
                },
                SongPart {
                    id: "bass".into(),
                    name: "Bass".into(),
                    role: "chord:bass".into(),
                    instrument: "bass".into(),
                    text: Some("gonna".into()),
                    octave: 0,
                },
            ],
        })
    }

    async fn start_ytpmv(
        &self,
        _service: &AppIdentity,
        _midi: &MidiFile,
        parts: &[PartChoice],
        options: &serde_json::Value,
        voice: Option<&str>,
    ) -> BotResult<String> {
        self.ytpmv
            .lock()
            .unwrap()
            .push((parts.to_vec(), options.clone(), voice.map(str::to_string)));
        Ok("job-2".into())
    }

    async fn list_voices(&self) -> BotResult<Vec<Voice>> {
        Ok(self.voices.lock().unwrap().clone())
    }

    async fn switch_voice(&self, _service: &AppIdentity, name: &str) -> BotResult<Voice> {
        *self.switched.lock().unwrap() = Some(name.to_string());
        Ok(Voice {
            name: name.to_string(),
            words: Some(7),
            active: true,
        })
    }

    async fn queue_status(&self) -> BotResult<QueueStatus> {
        Ok(QueueStatus {
            running: 1,
            queued: 2,
        })
    }

    async fn wait_job(
        &self,
        job_id: &str,
        on_progress: &(dyn for<'a> Fn(&'a JobProgress) + Send + Sync),
    ) -> BotResult<FinishedJob> {
        let progress = JobProgress {
            state: "running".into(),
            progress: 0.5,
            message: Some("singing".into()),
        };
        on_progress(&progress);
        let report = if job_id == "job-2" {
            serde_json::json!({
                "parts": [{"name": "Lead (piano)", "text": "never", "mute": false}],
                "problems": []
            })
        } else {
            serde_json::json!({ "missing": [] })
        };
        Ok(FinishedJob {
            job_id: job_id.into(),
            report,
            duration: Some(2.0),
        })
    }

    async fn encode_result(
        &self,
        _service: &AppIdentity,
        job_id: &str,
        preset: &str,
    ) -> BotResult<EncodedFile> {
        Ok(EncodedFile {
            path: PathBuf::from("fake.mp4"),
            filename: format!("{job_id}.mp4"),
            mime: "video/mp4".into(),
            bytes: preset.len() as u64,
        })
    }
}

#[derive(Default)]
struct FakeTransport {
    sent: Mutex<Vec<String>>,
    edited: Mutex<Vec<String>>,
    videos: Mutex<Vec<String>>,
    midi: Mutex<Option<MidiFile>>,
}

#[async_trait]
impl ChatTransport for FakeTransport {
    fn upload_preset(&self) -> String {
        "matrix".into()
    }

    async fn send(&self, _room: &str, text: &str, _reply_to: Option<&str>) -> BotResult<String> {
        self.sent.lock().unwrap().push(text.into());
        Ok(format!("m{}", self.sent.lock().unwrap().len()))
    }

    async fn edit(&self, _room: &str, _message_id: &str, text: &str) -> BotResult<()> {
        self.edited.lock().unwrap().push(text.into());
        Ok(())
    }

    async fn react(&self, _room: &str, _message_id: &str, _key: &str) {}

    async fn fetch_midi(&self, _midi: &MidiRef) -> BotResult<MidiFile> {
        self.midi
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| BotError::user("no midi"))
    }

    async fn send_video(
        &self,
        _room: &str,
        _file: &EncodedFile,
        caption: &str,
        _reply_to: Option<&str>,
    ) -> BotResult<()> {
        self.videos.lock().unwrap().push(caption.into());
        Ok(())
    }
}

fn identity() -> AppIdentity {
    AppIdentity {
        username: "bot".into(),
        roles: vec!["admin".into()],
        permissions: BTreeSet::new(),
        quota: quota(60),
    }
}

fn core(backend: Arc<FakeBackend>, policy: BotPolicy) -> BotCore {
    BotCore::new(backend, policy, identity(), CoreConfig::default())
}

fn msg(text: &str) -> ChatEvent {
    ChatEvent {
        platform: Platform::Matrix,
        room: "!room:example.org".into(),
        user_id: "@friend:example.org".into(),
        user_name: "friend".into(),
        direct: false,
        text: text.into(),
        event_id: Some("$1".into()),
        reply_to: None,
        midi: None,
        role_ids: Vec::new(),
        guild: None,
    }
}

fn open_policy() -> BotPolicy {
    BotPolicy {
        default_role: "guest".into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn say_runs_the_job_and_uploads_it() {
    let backend = FakeBackend::new(&[Permission::GenerateYtp], 60);
    let transport = FakeTransport::default();
    core(backend.clone(), open_policy())
        .handle(msg("!ytp say nice cake"), &transport)
        .await;

    assert_eq!(transport.videos.lock().unwrap().len(), 1);
    assert_eq!(transport.videos.lock().unwrap()[0], "nice cake");
    assert!(transport.edited.lock().unwrap().iter().any(|e| e.contains("✅")));
    let ytp = backend.ytp.lock().unwrap();
    assert_eq!(ytp.len(), 1);
    assert_eq!(ytp[0].0, "nice cake");
    assert!(ytp[0].1, "captions are on unless --no-subs");
}

#[tokio::test]
async fn mv_remembers_a_midi_and_renders_the_asked_parts() {
    let backend = FakeBackend::new(
        &[Permission::GenerateYtpmv, Permission::ExportRender],
        60,
    );
    let transport = FakeTransport::default();
    *transport.midi.lock().unwrap() = Some(MidiFile {
        name: "song.mid".into(),
        bytes: vec![1, 2, 3],
    });
    let core = core(backend.clone(), open_policy());

    let mut posted = msg("");
    posted.midi = Some(MidiRef {
        name: "song.mid".into(),
        key: "$file".into(),
    });
    core.handle(posted, &transport).await;

    let mut ask = msg("!ytp mv lead=yeah max=30");
    ask.reply_to = None;
    core.handle(ask, &transport).await;

    let jobs = backend.ytpmv.lock().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].0.len(), 1);
    assert_eq!(jobs[0].0[0].id, "lead");
    assert_eq!(jobs[0].0[0].settings["text"], "yeah");
    assert_eq!(jobs[0].1["max_seconds"], 30.0);
    assert_eq!(transport.videos.lock().unwrap()[0], "Never Gonna Give You Up — Lead: never");
}

#[tokio::test]
async fn voice_switching_needs_corpus_edit() {
    let backend = FakeBackend::new(&[Permission::CorpusRead], 60);
    let transport = FakeTransport::default();
    core(backend, open_policy())
        .handle(msg("!ytp voice other"), &transport)
        .await;

    assert!(transport
        .sent
        .lock()
        .unwrap()
        .iter()
        .any(|m| m.contains("missing corpus.edit")));
    assert!(transport.videos.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_rate_limit_is_per_chat_user() {
    let backend = FakeBackend::new(&[Permission::GenerateYtp], 1);
    let transport = FakeTransport::default();
    let core = core(backend, open_policy());

    core.handle(msg("!ytp say one"), &transport).await;
    core.handle(msg("!ytp say two"), &transport).await;

    assert_eq!(transport.videos.lock().unwrap().len(), 1);
    assert!(transport
        .sent
        .lock()
        .unwrap()
        .iter()
        .any(|m| m.starts_with("❌ Give it")));
}

#[tokio::test]
async fn outsiders_are_ignored_entirely() {
    let backend = FakeBackend::new(&[Permission::GenerateYtp], 60);
    let transport = FakeTransport::default();
    let mut policy = open_policy();
    policy.matrix = MatrixAccess {
        users: ["@friend:example.org".to_string()].into_iter().collect(),
        ..Default::default()
    };
    let mut outsider = msg("!ytp say nice cake");
    outsider.user_id = "@stranger:example.org".into();
    core(backend, policy)
        .handle(outsider, &transport)
        .await;

    assert!(transport.sent.lock().unwrap().is_empty());
    assert!(transport.videos.lock().unwrap().is_empty());
}
