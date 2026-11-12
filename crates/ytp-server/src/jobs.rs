//! The job system: a queue with per-user concurrency (from the role quota),
//! worker tasks, cancellation, progress events on the broadcast bus, and
//! the generation pipeline itself (corpus -> pool -> resolve -> render).
//!
//! Jobs live in memory; finished ones are also written beside their result
//! (`<data>/output/<id>.json`) so they survive a restart.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{broadcast, Notify};
use ytp_auth::{Principal, Quota};
use ytp_core::time::{now, rfc3339};
use ytp_core::{Error, Event, JobState, Result};
use ytp_corpus::data::Version;
use ytp_media::{RenderError, RenderJob};
use ytp_splice::{
    resolve_text, seeded_rng, timeline, total_duration, AudioCache, Cancelled, GenerationOptions,
    Pool, Runtime,
};

use crate::AppState;

const MAX_TEXT_CHARS: usize = 4000;
const KEEP_FINISHED: usize = 200;
const KEEP_FOR: i64 = 7 * 24 * 3600;
const MAX_VOICES_PER_TEXT: usize = 8;
const REMOVED_MSG_VOCODE: &str =
    "the {vocode} effect was removed (the vocoder is being reworked); take it out of the text";

/// What the API shows about a job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: String,
    pub owner: i64,
    pub owner_name: String,
    /// `"ytpmv"` for a song render; absent for a sentence.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub kind: Option<String>,
    pub state: JobState,
    pub progress: f32,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub message: Option<String>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub finished_at: Option<String>,
    pub text: String,
    pub voice: String,
    pub captions: bool,
    /// The options as given, then (once running) as used, seed included.
    pub options: Value,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub report: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub timeline: Option<Value>,
    /// Length of the finished video in seconds.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub duration: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub error: Option<String>,
    /// File name of the result under the output directory; absent means
    /// `<id>.mp4` (encode jobs can be `.gif`, `.m4a`, `.ogg` or `.mkv`).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub result_file: Option<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub result_url: Option<String>,
}

impl JobRecord {
    fn active(&self) -> bool {
        matches!(self.state, JobState::Queued | JobState::Running)
    }
}

pub struct NewJob {
    pub text: String,
    pub voice: String,
    pub captions: bool,
    pub options: Value,
}

struct Entry {
    rec: JobRecord,
    quota: Quota,
    cancel: Arc<AtomicBool>,
    last_emitted: f32,
}

#[derive(Default)]
struct Inner {
    map: HashMap<String, Entry>,
    queue: VecDeque<String>,
}

pub enum CancelOutcome {
    /// A queued job was cancelled on the spot.
    Cancelled,
    /// A running job was told to stop; it ends shortly.
    Stopping,
    /// A finished job was deleted along with its result.
    Removed,
}

pub struct Jobs {
    inner: Mutex<Inner>,
    notify: Notify,
    dir: PathBuf,
    events: broadcast::Sender<Event>,
    #[allow(clippy::type_complexity)]
    pools: Mutex<HashMap<String, (Option<Version>, Arc<Pool>)>>,
    pub workers: usize,
}

fn new_id() -> String {
    let (a, b): (u64, u64) = (rand::random(), rand::random());
    format!("{a:016x}{:04x}", b & 0xffff)
}

/// The job id a file in the output directory belongs to: `<id>.mp4`,
/// `<id>.master.mp4`, `<id>.gif`, `<id>.master.mp4.part0.mp4`…
fn output_id(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_string_lossy();
    let (id, rest) = name.split_once('.')?;
    if id.len() == 20 && id.bytes().all(|b| b.is_ascii_hexdigit()) && !rest.is_empty() {
        Some(id.to_string())
    } else {
        None
    }
}

impl Jobs {
    pub fn new(data_dir: &Path, workers: usize, events: broadcast::Sender<Event>) -> Self {
        let dir = data_dir.join("output");
        let _ = std::fs::create_dir_all(&dir);
        let mut inner = Inner::default();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "json") {
                    let Some(rec) = std::fs::read(&p)
                        .ok()
                        .and_then(|b| serde_json::from_slice::<JobRecord>(&b).ok())
                    else {
                        continue;
                    };
                    if rec.active() {
                        continue;
                    }
                    inner.map.insert(
                        rec.id.clone(),
                        Entry {
                            rec,
                            quota: Quota::ZERO,
                            cancel: Arc::default(),
                            last_emitted: 1.0,
                        },
                    );
                } else if let Some(id) = output_id(&p) {
                    // A result or master whose sidecar never got written (a
                    // crash), or a chunk left behind by an interrupted join.
                    if !dir.join(format!("{id}.json")).exists() {
                        let _ = std::fs::remove_file(&p);
                    }
                }
            }
        }
        Self {
            inner: Mutex::new(inner),
            notify: Notify::new(),
            dir,
            events,
            pools: Mutex::new(HashMap::new()),
            workers: workers.max(1),
        }
    }

    fn emit(&self, rec: &JobRecord) {
        let _ = self.events.send(Event::Job {
            id: rec.id.clone(),
            owner: rec.owner,
            state: rec.state,
            progress: rec.progress,
            message: rec.message.clone().or_else(|| rec.error.clone()),
        });
    }

    pub fn result_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.mp4"))
    }

    /// The file a finished job's result is in (`<id>.mp4` for sentence and
    /// song renders, the preset's container for an encode).
    pub fn result_path_of(&self, rec: &JobRecord) -> PathBuf {
        match &rec.result_file {
            Some(file) => self.dir.join(file),
            None => self.result_path(&rec.id),
        }
    }

    /// A file in the output directory, by name.
    pub fn output_path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn sidecar(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    pub fn get(&self, id: &str) -> Option<JobRecord> {
        self.inner.lock().unwrap().map.get(id).map(|e| e.rec.clone())
    }

    /// How many jobs are running and how many are waiting.
    pub fn counts(&self) -> (usize, usize) {
        let g = self.inner.lock().unwrap();
        let running = g.map.values().filter(|e| e.rec.state == JobState::Running).count();
        (running, g.queue.len())
    }

    /// Is an unfinished render of `owner` playing MIDI file `midi_id`?
    pub fn midi_in_use(&self, owner: i64, midi_id: &str) -> bool {
        self.inner.lock().unwrap().map.values().any(|e| {
            e.rec.active()
                && e.rec.owner == owner
                && e.rec.kind.as_deref() == Some("ytpmv")
                && e.rec.options["midi_id"] == midi_id
        })
    }

    /// Queue a job for `owner`.
    pub fn submit(&self, owner: &Principal, spec: NewJob) -> Result<JobRecord> {
        self.submit_as(owner, spec, None)
    }

    /// Queue a job of the given kind (`None` is a sentence) for `owner`.
    pub fn submit_as(&self, owner: &Principal, spec: NewJob, kind: Option<&str>) -> Result<JobRecord> {
        let q = owner.quota;
        if q.max_concurrent_jobs == 0 {
            return Err(Error::forbidden("your role may not run jobs"));
        }
        if q.max_render_seconds == 0 {
            return Err(Error::forbidden("your role may not render video"));
        }
        let mut g = self.inner.lock().unwrap();
        let active = g
            .map
            .values()
            .filter(|e| e.rec.owner == owner.user.id && e.rec.active())
            .count();
        if active >= (q.max_concurrent_jobs as usize * 4).max(4) {
            return Err(Error::RateLimited);
        }
        let id = new_id();
        let rec = JobRecord {
            id: id.clone(),
            owner: owner.user.id,
            owner_name: owner.user.username.clone(),
            kind: kind.map(str::to_string),
            state: JobState::Queued,
            progress: 0.0,
            message: Some("queued".into()),
            created_at: rfc3339(now()),
            started_at: None,
            finished_at: None,
            text: spec.text,
            voice: spec.voice,
            captions: spec.captions,
            options: spec.options,
            report: None,
            timeline: None,
            duration: None,
            size_bytes: None,
            error: None,
            result_file: None,
            warnings: Vec::new(),
            result_url: None,
        };
        self.emit(&rec);
        g.queue.push_back(id.clone());
        g.map.insert(
            id,
            Entry {
                rec: rec.clone(),
                quota: q,
                cancel: Arc::default(),
                last_emitted: 0.0,
            },
        );
        drop(g);
        self.notify.notify_waiters();
        Ok(rec)
    }

    /// Cancel an active job, or delete a finished one.
    pub fn cancel(&self, id: &str) -> Option<CancelOutcome> {
        let mut g = self.inner.lock().unwrap();
        let e = g.map.get_mut(id)?;
        match e.rec.state {
            JobState::Queued => {
                e.rec.state = JobState::Cancelled;
                e.rec.message = Some("cancelled".into());
                e.rec.finished_at = Some(rfc3339(now()));
                let rec = e.rec.clone();
                g.queue.retain(|q| q != id);
                self.emit(&rec);
                Some(CancelOutcome::Cancelled)
            }
            JobState::Running => {
                e.cancel.store(true, Ordering::Relaxed);
                e.rec.message = Some("cancelling…".into());
                let rec = e.rec.clone();
                self.emit(&rec);
                Some(CancelOutcome::Stopping)
            }
            _ => {
                let file = e.rec.result_file.clone();
                g.map.remove(id);
                drop(g);
                self.delete_files(id, file.as_deref());
                Some(CancelOutcome::Removed)
            }
        }
    }

    fn delete_files(&self, id: &str, result_file: Option<&str>) {
        let _ = std::fs::remove_file(self.result_path(id));
        let _ = std::fs::remove_file(ytp_media::master_path(&self.result_path(id)));
        if let Some(file) = result_file {
            let _ = std::fs::remove_file(self.dir.join(file));
        }
        let _ = std::fs::remove_file(self.sidecar(id));
    }

    /// The next queued job whose owner is under their concurrency quota,
    /// marked running.
    fn pick_next(&self) -> Option<(String, Arc<AtomicBool>)> {
        let mut g = self.inner.lock().unwrap();
        let mut running: HashMap<i64, u32> = HashMap::new();
        for e in g.map.values() {
            if e.rec.state == JobState::Running {
                *running.entry(e.rec.owner).or_default() += 1;
            }
        }
        let pos = g.queue.iter().position(|id| {
            g.map.get(id).is_some_and(|e| {
                running.get(&e.rec.owner).copied().unwrap_or(0) < e.quota.max_concurrent_jobs
            })
        })?;
        let id = g.queue.remove(pos)?;
        let e = g.map.get_mut(&id)?;
        e.rec.state = JobState::Running;
        e.rec.started_at = Some(rfc3339(now()));
        e.rec.message = Some("starting".into());
        let rec = e.rec.clone();
        let cancel = e.cancel.clone();
        self.emit(&rec);
        Some((id, cancel))
    }

    pub(crate) fn set_progress(&self, id: &str, progress: f32, message: String) {
        let mut g = self.inner.lock().unwrap();
        let Some(e) = g.map.get_mut(id) else { return };
        if e.rec.state != JobState::Running || e.cancel.load(Ordering::Relaxed) {
            return;
        }
        let changed = (progress - e.last_emitted).abs() >= 0.01
            || e.rec.message.as_deref() != Some(message.as_str());
        e.rec.progress = progress;
        e.rec.message = Some(message);
        if changed {
            e.last_emitted = progress;
            let rec = e.rec.clone();
            self.emit(&rec);
        }
    }

    async fn finish(&self, id: &str, outcome: Outcome) {
        let rec = {
            let mut g = self.inner.lock().unwrap();
            let Some(e) = g.map.get_mut(id) else { return };
            let r = &mut e.rec;
            r.finished_at = Some(rfc3339(now()));
            r.warnings = outcome.warnings;
            if let Some(opts) = outcome.options {
                r.options = opts;
            }
            if outcome.report.is_some() {
                r.report = outcome.report;
            }
            match outcome.result {
                Ok(done) => {
                    r.state = JobState::Done;
                    r.progress = 1.0;
                    r.message = Some("done".into());
                    r.timeline = Some(done.timeline);
                    r.duration = Some(done.duration);
                    r.size_bytes = Some(done.size_bytes);
                    r.result_file = done.file;
                    r.result_url = Some(format!("/api/jobs/{id}/result"));
                }
                Err(Failure::Cancelled) => {
                    r.state = JobState::Cancelled;
                    r.message = Some("cancelled".into());
                }
                Err(Failure::Failed(msg)) => {
                    r.state = JobState::Failed;
                    r.message = Some(msg.clone());
                    r.error = Some(msg);
                }
            }
            e.last_emitted = r.progress;
            r.clone()
        };
        if rec.state != JobState::Done {
            self.delete_files(id, rec.result_file.as_deref());
        }
        if let Ok(bytes) = serde_json::to_vec(&rec) {
            let _ = tokio::fs::write(self.sidecar(id), bytes).await;
        }
        self.emit(&rec);
        self.prune();
        self.notify.notify_waiters();
    }

    /// Forget old finished jobs and their files.
    fn prune(&self) {
        let cutoff = now() - KEEP_FOR;
        let mut finished: Vec<(String, String, Option<String>)> = {
            let g = self.inner.lock().unwrap();
            g.map
                .values()
                .filter(|e| !e.rec.active())
                .map(|e| (e.rec.id.clone(), e.rec.finished_at.clone().unwrap_or_default(), e.rec.result_file.clone()))
                .collect()
        };
        finished.sort_by(|a, b| b.1.cmp(&a.1));
        let old_cut = rfc3339(cutoff);
        let drop_ids: Vec<(String, Option<String>)> = finished
            .iter()
            .enumerate()
            .filter(|(i, (_, at, _))| *i >= KEEP_FINISHED || at.as_str() < old_cut.as_str())
            .map(|(_, (id, _, file))| (id.clone(), file.clone()))
            .collect();
        if drop_ids.is_empty() {
            return;
        }
        let mut g = self.inner.lock().unwrap();
        for (id, file) in &drop_ids {
            g.map.remove(id);
            self.delete_files(id, file.as_deref());
        }
    }

    /// The pool for corpus `name`, built once per version of its database.
    pub(crate) async fn pool(&self, st: &AppState, name: &str) -> Result<Arc<Pool>> {
        let version = st.corpora.version(name).ok_or_else(|| Error::not_found("no such corpus"))?;
        if let Some((m, p)) = self.pools.lock().unwrap().get(name) {
            if *m == Some(version) {
                return Ok(p.clone());
            }
        }
        let data = st.corpora.load(name).await?;
        let pool = tokio::task::spawn_blocking(move || Pool::build(&data))
            .await
            .map_err(Error::internal)?;
        let pool = Arc::new(pool);
        let mut cache = self.pools.lock().unwrap();
        cache.retain(|n, _| st.corpora.dir(n).is_dir());
        cache.insert(name.to_string(), (Some(version), pool.clone()));
        Ok(pool)
    }
}

pub(crate) struct Done {
    pub timeline: Value,
    pub duration: f64,
    pub size_bytes: u64,
    /// File name under the output directory, when it is not `<id>.mp4`.
    pub file: Option<String>,
}

pub(crate) enum Failure {
    Cancelled,
    Failed(String),
}

pub(crate) struct Outcome {
    pub result: std::result::Result<Done, Failure>,
    pub report: Option<Value>,
    pub options: Option<Value>,
    pub warnings: Vec<String>,
}

impl Outcome {
    fn failed(msg: impl Into<String>) -> Self {
        Outcome {
            result: Err(Failure::Failed(msg.into())),
            report: None,
            options: None,
            warnings: Vec::new(),
        }
    }
}

fn stage_progress(stage: &str, done: usize, total: usize) -> (f32, String) {
    let frac = if total == 0 { 0.0 } else { (done as f32 / total as f32).min(1.0) };
    match stage {
        "loading" => (0.02 * frac, "loading the corpus".into()),
        "resolving" => (0.02 + 0.38 * frac, format!("choosing clips {done}/{total}")),
        "encoding" if total > 1 => (
            0.40 + 0.55 * frac,
            format!("encoding part {}/{total}", (done + 1).min(total)),
        ),
        "encoding" => (0.40 + 0.55 * frac, "encoding".into()),
        "joining" => (0.96, "joining the parts".into()),
        "finishing" => (0.95 + 0.04 * frac, "compressing".into()),
        other => (0.0, other.to_string()),
    }
}

/// Validate and normalise a generate request.
pub fn check_text(text: &str) -> Result<String> {
    let t = text.trim();
    if t.is_empty() {
        return Err(Error::invalid("text is empty"));
    }
    if t.chars().count() > MAX_TEXT_CHARS {
        return Err(Error::invalid(format!(
            "text is longer than {MAX_TEXT_CHARS} characters"
        )));
    }
    if ytp_text::tokenize_full(t, &ytp_text::NoLexicon).iter().any(|tok| tok.fx.vocode) {
        return Err(Error::invalid(REMOVED_MSG_VOCODE));
    }
    Ok(t.to_string())
}

/// Reject options that were removed (sing, vocode) instead of ignoring them.
pub fn check_options(options: &Value) -> Result<()> {
    let gone = ytp_splice::removed_options(options);
    if gone.is_empty() {
        return Ok(());
    }
    Err(Error::invalid(format!(
        "the option(s) {} were removed (singing and the vocoder are being reworked); leave them out",
        gone.join(", ")
    )))
}

/// Start the worker tasks.
pub fn start_workers(st: &AppState) {
    for _ in 0..st.jobs.workers {
        let st = st.clone();
        tokio::spawn(async move {
            loop {
                let next = st.jobs.pick_next();
                let Some((id, cancel)) = next else {
                    tokio::select! {
                        _ = st.jobs.notify.notified() => {}
                        _ = tokio::time::sleep(Duration::from_millis(500)) => {}
                    }
                    continue;
                };
                let outcome = run(&st, &id, cancel).await;
                st.jobs.finish(&id, outcome).await;
            }
        });
    }
}

async fn run(st: &AppState, id: &str, cancel: Arc<AtomicBool>) -> Outcome {
    let Some(entry) = st.jobs.inner.lock().unwrap().map.get(id).map(|e| (e.rec.clone(), e.quota))
    else {
        return Outcome::failed("job vanished");
    };
    let (rec, quota) = entry;
    if cancel.load(Ordering::Relaxed) {
        return Outcome { result: Err(Failure::Cancelled), report: None, options: None, warnings: vec![] };
    }
    if rec.kind.as_deref() == Some("ytpmv") {
        return crate::ytpmv::run(st, id, &rec, quota, cancel).await;
    }
    if rec.kind.as_deref() == Some("encode") {
        return crate::encode::run(st, id, &rec, quota, cancel).await;
    }
    if rec.kind.as_deref() == Some("project") {
        return crate::projects::run(st, id, &rec, quota, cancel).await;
    }

    st.jobs.set_progress(id, 0.0, "loading the corpus".into());
    let pool = match st.jobs.pool(st, &rec.voice).await {
        Ok(p) => p,
        Err(e) => return Outcome::failed(format!("voice {:?}: {e}", rec.voice)),
    };
    // Other voices the text borrows with word{v=name}.
    let mut voices: HashMap<String, Arc<Pool>> = HashMap::new();
    let wanted: Vec<String> = {
        let mut v: Vec<String> = ytp_text::tokenize_full(&rec.text, &ytp_text::NoLexicon)
            .into_iter()
            .filter_map(|t| t.fx.voice)
            .collect();
        v.sort();
        v.dedup();
        v.truncate(MAX_VOICES_PER_TEXT);
        v
    };
    for name in wanted {
        if name == rec.voice {
            continue;
        }
        if let Ok(p) = st.jobs.pool(st, &name).await {
            voices.insert(name, p);
        }
    }

    let st2 = st.clone();
    let id2 = id.to_string();
    let out_path = st.jobs.result_path(id);
    let joined = tokio::task::spawn_blocking(move || {
        generate(&st2, &id2, &rec, quota, pool, voices, &out_path, &cancel)
    })
    .await;
    match joined {
        Ok(o) => o,
        Err(e) => Outcome::failed(format!("the generator crashed: {e}")),
    }
}

#[allow(clippy::too_many_arguments)]
fn generate(
    st: &AppState,
    id: &str,
    rec: &JobRecord,
    quota: Quota,
    pool: Arc<Pool>,
    voices: HashMap<String, Arc<Pool>>,
    out: &Path,
    cancel: &AtomicBool,
) -> Outcome {
    let mut opts = GenerationOptions::from_json(&rec.options);
    let mut rng = seeded_rng(&mut opts);
    let mut warnings = Vec::new();
    let _pronunciations = pool.scope();
    let progress = |stage: &str, done: usize, total: usize| -> bool {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let (p, msg) = stage_progress(stage, done, total);
        st.jobs.set_progress(id, p, msg);
        true
    };
    let fail = |msg: String, report: Option<Value>, opts: &GenerationOptions, warnings: Vec<String>| Outcome {
        result: Err(Failure::Failed(msg)),
        report,
        options: Some(opts.to_json()),
        warnings,
    };
    let cancelled = |opts: &GenerationOptions, warnings: Vec<String>| Outcome {
        result: Err(Failure::Cancelled),
        report: None,
        options: Some(opts.to_json()),
        warnings,
    };

    let audio = AudioCache::new(st.renderer.audio());
    let lookup = |name: &str| voices.get(name).cloned();
    let rt = Runtime {
        pool,
        voices: &lookup,
        audio: &audio,
        glitches: st.renderer.glitches(),
    };
    let (segments, report) = match resolve_text(&rec.text, &opts, &rt, &mut rng, &progress) {
        Ok(r) => r,
        Err(Cancelled) => return cancelled(&opts, warnings),
    };
    let report_json = report.to_json();
    if segments.is_empty() {
        let missing = report.missing.join(", ");
        let msg = if missing.is_empty() {
            "there is nothing to say".to_string()
        } else {
            format!("none of the words could be said: {missing}")
        };
        return fail(msg, Some(report_json), &opts, warnings);
    }

    let duration = total_duration(&segments, &opts, &audio);
    if duration > quota.max_render_seconds as f64 {
        return fail(
            format!(
                "the video would be {duration:.1} s long; your limit is {} s",
                quota.max_render_seconds
            ),
            Some(report_json),
            &opts,
            warnings,
        );
    }
    if !report.missing.is_empty() {
        warnings.push(format!("missing words: {}", report.missing.join(", ")));
    }
    let spans = timeline(&segments, &opts, &audio);

    let job = RenderJob {
        segments: &segments,
        options: &opts,
        captions: rec.captions,
        out,
        audio: &audio,
        max_height: quota.max_height,
        encoder: Some(st.encoder.as_ref()),
        finish: true,
        cancel: Some(&|| cancel.load(Ordering::Relaxed)),
    };
    match st.renderer.render(&job, &progress) {
        Ok(()) => {}
        Err(RenderError::Cancelled) => return cancelled(&opts, warnings),
        Err(e) => return fail(e.to_string(), Some(report_json), &opts, warnings),
    }
    let size_bytes = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    Outcome {
        result: Ok(Done {
            timeline: Value::Array(spans.iter().map(|s| s.to_json()).collect()),
            duration: (duration * 1000.0).round() / 1000.0,
            size_bytes,
            file: None,
        }),
        report: Some(report_json),
        options: Some(opts.to_json()),
        warnings,
    }
}

/// Shape of the 202/200 reply to a generate request.
pub fn accepted(rec: &JobRecord) -> Value {
    json!({ "job_id": rec.id })
}
