//! Timeline projects on the server: planning a project from a generator
//! (without rendering), the render job, and the low-res preview of a time
//! window with its disk caches.
//!
//! * `plan_ytp` / `plan_ytpmv` run the generators' planning halves and answer
//!   with a [`Project`].
//! * `run` is the `project` job: the full-quality render through
//!   `ytp-render` (sequence projects) or the song renderer (song projects).
//! * `preview` renders a window quickly. Sequence projects go through one
//!   fast ffmpeg call over proxy media; song projects play only the window of
//!   the song through the same mixer and picture code as the full render.
//!   Finished windows are cached on disk by what is in them.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use ytp_auth::Quota;
use ytp_core::{Clip, ClipSource, Error, GeneratorKind, Project, Result, TrackKind};
use ytp_media::FfmpegRenderer;
use ytp_render::{Env, FullOptions, Kind, ProxyCache};
use ytp_splice::{
    project_from_plan, resolve_text, seeded_rng, AudioCache, Cancelled, GenerationOptions, Locator, PlanInfo, Pool,
    Runtime,
};
use ytp_ytpmv::mix::{self, Request};
use ytp_ytpmv::samples::Banks;
use ytp_ytpmv::settings::Options;
use ytp_ytpmv::{video, MAX_SECONDS};

use crate::jobs::{Done, Failure, JobRecord, Outcome};
use crate::ytpmv::api_error;
use crate::AppState;

/// Longest window one preview renders, seconds.
pub const PREVIEW_MAX_SECONDS: f64 = 30.0;
/// Largest project document accepted.
pub const MAX_PROJECT_BYTES: usize = 16 << 20;
const PREVIEW_CACHE_BYTES: u64 = 256 << 20;
const PROXY_CACHE_BYTES: u64 = 2048 << 20;
const CONCURRENT_PREVIEWS: usize = 3;
/// Height of a song preview's picture.
const PREVIEW_HEIGHT: u32 = 270;
/// Bump to drop every cached preview after a rendering change.
const PREVIEW_VERSION: u32 = 1;

/// Scratch names a render writes into the work directory, as prefix and
/// suffix: the compositor's `pip-<pid>-<n>.mp4` and `pip-<k>-<pid>-<n>.mp4`,
/// the filler `filler-12s.mp4` and its `filler-12s.<pid>.tmp.mp4`, and the
/// synthesised `synth-<key>.wav` with its `synth-<key>.<pid>.tmp`.
const SCRATCH_NAMES: &[(&str, &str)] = &[("pip-", ".mp4"), ("filler-", ".mp4"), ("synth-", ".wav"), ("synth-", ".tmp")];

/// Whether `name` is one a render makes for itself in the work directory.
fn is_scratch_name(name: &str) -> bool {
    SCRATCH_NAMES.iter().any(|(prefix, suffix)| {
        name.strip_prefix(prefix).and_then(|rest| rest.strip_suffix(suffix)).is_some_and(|mid| !mid.is_empty())
    })
}

/// Remove the scratch files an earlier server process left in `dir`, answering
/// with how many went. A plan removes its own when it is dropped, so a file
/// still there belongs to a process that died; a name no render makes is left
/// alone, and so is a file that will not go.
pub fn sweep_scratch(dir: &Path) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        if !is_scratch_name(name) {
            continue;
        }
        if std::fs::remove_file(e.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

pub struct Service {
    pub renderer: FfmpegRenderer,
    pub proxy: Arc<ProxyCache>,
    ffmpeg: PathBuf,
    work_dir: PathBuf,
    preview_dir: PathBuf,
    media_dir: PathBuf,
    previews: Semaphore,
}

impl Service {
    pub fn new(data_dir: &Path, ffmpeg: PathBuf, encoder: Arc<ytp_encode::Encoder>) -> Self {
        let cache = data_dir.join("cache");
        let preview_dir = cache.join("preview");
        // A preview being written when the server stopped.
        if let Ok(rd) = std::fs::read_dir(&preview_dir) {
            for e in rd.flatten() {
                if e.path().to_string_lossy().ends_with(".tmp.mp4") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        let work_dir = cache.join("project-work");
        // A render clears its own scratch, so what is left in the work
        // directory is from a process that died; nothing is running yet.
        let swept = sweep_scratch(&work_dir);
        tracing::info!(files = swept, dir = %work_dir.display(), "cleared scratch a previous server left in the work directory");
        Self {
            renderer: FfmpegRenderer::new(ffmpeg.clone()),
            proxy: ProxyCache::new(cache.join("proxy"), PROXY_CACHE_BYTES, encoder),
            ffmpeg,
            work_dir,
            preview_dir,
            media_dir: data_dir.join("media"),
            previews: Semaphore::new(CONCURRENT_PREVIEWS),
        }
    }

    fn env<'a>(&'a self, locator: &'a Locator) -> Env<'a> {
        Env { renderer: &self.renderer, locator, media_dir: Some(&self.media_dir), work_dir: &self.work_dir }
    }
}

/// Where each installed corpus lives.
pub async fn locator(st: &AppState) -> Result<Locator> {
    let dirs = st.corpora.list().await?.into_iter().map(|c| (st.corpora.dir(&c.name), c.name)).map(|(d, n)| (n, d)).collect();
    Ok(Locator::new(dirs))
}

/// A project document from JSON, checked.
pub fn parse_project(v: Value) -> Result<Project> {
    let p: Project = serde_json::from_value(v).map_err(|e| Error::invalid(format!("not a project: {e}")))?;
    p.validate().map_err(Error::invalid)?;
    Ok(p)
}

pub fn kind_name(p: &Project) -> &'static str {
    match ytp_render::kind(p) {
        Kind::Sequence => "sequence",
        Kind::Song => "song",
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f).await.map_err(Error::internal)?
}

// ── Planning ─────────────────────────────────────────────────────────────────

/// Choose the clips for a sentence, as a request to `/api/ytp/generate` would,
/// and answer with the plan as a project (no rendering). The word report goes
/// with it.
pub async fn plan_ytp(st: &AppState, text: String, voice: String, captions: bool, options: Value) -> Result<(Project, Value)> {
    let pool = st.jobs.pool(st, &voice).await.map_err(|e| Error::invalid(format!("voice {voice:?}: {e}")))?;
    let mut voices: HashMap<String, Arc<Pool>> = HashMap::new();
    let mut wanted: Vec<String> = ytp_text::tokenize_full(&text, &ytp_text::NoLexicon).into_iter().filter_map(|t| t.fx.voice).collect();
    wanted.sort();
    wanted.dedup();
    wanted.truncate(8);
    for name in wanted {
        if name != voice {
            if let Ok(p) = st.jobs.pool(st, &name).await {
                voices.insert(name, p);
            }
        }
    }
    let locator = locator(st).await?;
    let st2 = st.clone();
    blocking(move || {
        let mut opts = GenerationOptions::from_json(&options);
        let mut rng = seeded_rng(&mut opts);
        let _scope = pool.scope();
        let audio = AudioCache::new(st2.renderer.audio());
        let lookup = |name: &str| voices.get(name).cloned();
        let rt = Runtime { pool, voices: &lookup, audio: &audio, glitches: st2.renderer.glitches() };
        let (segments, report) = match resolve_text(&text, &opts, &rt, &mut rng, &|_, _, _| true) {
            Ok(r) => r,
            Err(Cancelled) => return Err(Error::conflict("cancelled")),
        };
        if segments.is_empty() {
            let missing = report.missing.join(", ");
            return Err(Error::invalid(if missing.is_empty() {
                "there is nothing to say".to_string()
            } else {
                format!("none of the words could be said: {missing}")
            }));
        }
        let name: String = text.chars().filter(|c| !c.is_control()).take(60).collect();
        let request = json!({ "text": text, "voice": voice, "captions": captions, "options": opts.to_json() });
        let info = PlanInfo { name: name.trim(), voice: &voice, text: &text, captions, options: &opts, request };
        let project = project_from_plan(&segments, &audio, &info, &locator).map_err(Error::invalid)?;
        project.validate().map_err(Error::invalid)?;
        Ok((project, report.to_json()))
    })
    .await
}

/// The parts of a song request, as `/api/ytpmv/render` takes them.
pub struct SongRequest {
    pub midi_id: String,
    pub parts: Vec<Value>,
    pub options: Value,
    pub voice: String,
}

/// A song's render plan as a project: one track per part, one clip per note.
pub async fn plan_ytpmv(st: &AppState, owner: i64, req: SongRequest, max_height: u32) -> Result<Project> {
    let song = {
        let (st2, id) = (st.clone(), req.midi_id.clone());
        blocking(move || st2.ytpmv.load_song(owner, &id)).await?
    };
    let opts = Options::from_json(&req.options, MAX_SECONDS);
    let mut banks = Banks::new();
    if !opts.synth {
        for name in mix::corpora_of(&req.parts, &req.voice) {
            if name.is_empty() || banks.contains_key(&name) {
                continue;
            }
            let pool = st.jobs.pool(st, &name).await.map_err(|_| Error::invalid(format!("The corpus '{name}' is not installed. Choose another voice.")))?;
            banks.insert(name.clone(), st.ytpmv.bank(&name, pool));
        }
    }
    let st2 = st.clone();
    blocking(move || {
        let env = st2.ytpmv.env(&st2);
        let request = Request {
            song: &song,
            parts: &req.parts,
            options: opts,
            default_corpus: &req.voice,
            banks: &banks,
            env: &env,
            store: &st2.ytpmv.store,
            audition: None,
            audio_only: false,
            start_seconds: 0.0,
            only_part: None,
            out_size: ytp_ytpmv::fit_out_size(max_height),
        };
        let progress = |_: &str, _: usize, _: usize| true;
        let prep = mix::prepare(&request, &progress).map_err(api_error)?;
        let name = if song.title.is_empty() { format!("midi {}", req.midi_id) } else { song.title.clone() };
        let doc = json!({ "midi_id": req.midi_id, "parts": req.parts, "options": req.options, "voice": req.voice });
        ytp_ytpmv::plan::plan_project(&prep, &name, doc).map_err(Error::invalid)
    })
    .await
}

// ── Generated clips ──────────────────────────────────────────────────────────

fn is_generated(c: &Clip) -> bool {
    matches!(c.source, ClipSource::Generated { .. })
}

/// Expand every `generated` clip into the clips its prompt makes, for this
/// render only. The stored project keeps the generated clip untouched: nothing
/// is written back and no revision changes. That is safe because the expansion
/// is a function of the clip (its prompt, options and seed), so the same seed
/// always cuts the same clips.
///
/// The expansion's real length wins: it replaces the host clip's authored
/// `duration`, and every later clip on that track moves by the difference (in
/// either direction). Nothing is trimmed or padded to fit.
///
/// Depth is one: a plan that itself asks for a generated clip is refused, and
/// a generated clip on an audio track is refused for now.
pub async fn expand_generated_clips(st: &AppState, mut project: Project) -> Result<Project> {
    if !project.tracks.iter().any(|t| t.clips.iter().any(is_generated)) {
        return Ok(project);
    }
    let (host_corpus, captions) = (project.corpus.clone(), project.captions);
    let mut used: HashSet<String> = project.tracks.iter().flat_map(|t| t.clips.iter().map(|c| c.id.clone())).collect();
    for ti in 0..project.tracks.len() {
        if !project.tracks[ti].clips.iter().any(is_generated) {
            continue;
        }
        if project.tracks[ti].kind == TrackKind::Audio {
            let id = project.tracks[ti].clips.iter().find(|c| is_generated(c)).map_or("", |c| c.id.as_str());
            return Err(Error::invalid(format!("clip {id}: a generated clip needs a video track for now")));
        }
        // An expansion moves everything after it, so walk the track in timeline
        // order and carry the running difference.
        let mut order: Vec<usize> = (0..project.tracks[ti].clips.len()).collect();
        order.sort_by(|a, b| project.tracks[ti].clips[*a].start.total_cmp(&project.tracks[ti].clips[*b].start));
        let mut out: Vec<Clip> = Vec::with_capacity(project.tracks[ti].clips.len());
        let mut shift = 0.0;
        for ci in order {
            let clip = project.tracks[ti].clips[ci].clone();
            let start = clip.start + shift;
            let made = match &clip.source {
                ClipSource::Generated { kind: GeneratorKind::Ytp, prompt, options, seed, corpus } => {
                    // The clip's own seed is the one that counts; `plan_ytp`
                    // reads it from the options it is handed.
                    let mut options = options.clone();
                    if !seed.is_empty() {
                        match &mut options {
                            Value::Object(o) => {
                                o.insert("seed".into(), Value::String(seed.clone()));
                            }
                            _ => options = json!({ "seed": seed }),
                        }
                    }
                    let voice = corpus.clone().filter(|c| !c.is_empty()).unwrap_or_else(|| host_corpus.clone());
                    let (plan, _report) = plan_ytp(st, prompt.clone(), voice, captions, options)
                        .await
                        .map_err(|e| Error::invalid(format!("clip {}: {e}", clip.id)))?;
                    expansion_clips(&clip, start, &plan)?
                }
                ClipSource::Generated { .. } => {
                    return Err(Error::invalid(format!("clip {}: only a ytp generated clip can be expanded for now", clip.id)));
                }
                _ => {
                    let mut kept = clip;
                    kept.start = start;
                    out.push(kept);
                    continue;
                }
            };
            let end = made.iter().map(Clip::end).fold(start, f64::max);
            shift += (end - start) - clip.duration;
            for mut c in made {
                c.id = fresh_clip_id(&clip.id, &mut used);
                out.push(c);
            }
        }
        project.tracks[ti].clips = out;
    }
    // The checks downstream (MAX_CLIPS, quota seconds) see the expanded
    // project, so it has to be sound on its own.
    project.validate().map_err(Error::invalid)?;
    Ok(project)
}

/// The clips one expansion puts in place of `host`, from the plan its prompt
/// made: the plan's picture clips, moved to where the host sits (`start`, after
/// earlier expansions on the track have moved it).
///
/// Depth 1: a plan that itself asks for a generated clip is refused.
pub fn expansion_clips(host: &Clip, start: f64, plan: &Project) -> Result<Vec<Clip>> {
    if plan.tracks.iter().flat_map(|t| t.clips.iter()).any(is_generated) {
        return Err(Error::invalid("a generated clip cannot contain another generated clip yet"));
    }
    let made = plan
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video && !t.clips.is_empty())
        .ok_or_else(|| Error::invalid(format!("clip {}: its prompt made no picture to play", host.id)))?;
    let mut clips = made.clips.clone();
    clips.sort_by(|a, b| a.start.total_cmp(&b.start));
    for c in &mut clips {
        c.start += start;
    }
    Ok(clips)
}

/// A clip id derived from `host`'s, unique in `used`: two expansions cannot
/// collide with each other or with the plan's own ids.
fn fresh_clip_id(host: &str, used: &mut HashSet<String>) -> String {
    let stem: String = host.chars().take(48).collect();
    let mut k = 0usize;
    loop {
        let id = format!("{stem}_g{k}");
        if used.insert(id.clone()) {
            return id;
        }
        k += 1;
    }
}

// ── The render job ───────────────────────────────────────────────────────────

fn failed(msg: impl Into<String>) -> Outcome {
    Outcome { result: Err(Failure::Failed(msg.into())), report: None, options: None, warnings: Vec::new() }
}

fn stage_progress(stage: &str, done: usize, total: usize) -> (f32, String) {
    let frac = if total == 0 { 0.0 } else { (done as f32 / total as f32).min(1.0) };
    match stage {
        "encoding" if total > 1 => (0.05 + 0.80 * frac, format!("encoding part {}/{total}", (done + 1).min(total))),
        "encoding" => (0.05 + 0.80 * frac, "encoding".into()),
        "joining" => (0.88, "joining the parts".into()),
        "finishing" => (0.90 + 0.09 * frac, "compressing".into()),
        other => (0.02, other.to_string()),
    }
}

/// Run a queued project render: `rec.options` holds `project_id`.
pub(crate) async fn run(st: &AppState, id: &str, rec: &JobRecord, quota: Quota, cancel: Arc<AtomicBool>) -> Outcome {
    let Some(pid) = rec.options["project_id"].as_i64() else { return failed("this job does not say which project to render") };
    let stored = match st.auth.project_get(pid, rec.owner).await {
        Ok(Some(p)) => p,
        Ok(None) => return failed("the project has gone, or is no longer shared with you"),
        Err(e) => return failed(e.to_string()),
    };
    let mut project = match serde_json::from_str::<Project>(&stored.data).map_err(|e| e.to_string()).and_then(|p| p.validate().map(|_| p)) {
        Ok(p) => p,
        Err(e) => return failed(format!("the stored project is not valid: {e}")),
    };
    project.id = Some(pid);
    project.owner = Some(stored.meta.owner);
    // Generated clips become the clips their prompts make, for this render
    // only; the duration and clip-count checks below see the expanded project.
    let project = match expand_generated_clips(st, project).await {
        Ok(p) => p,
        Err(e) => return failed(e.to_string()),
    };
    let duration = project.duration();
    if duration > quota.max_render_seconds as f64 {
        return failed(format!("the video would be {duration:.1} s long; your limit is {} s", quota.max_render_seconds));
    }
    st.jobs.set_progress(id, 0.01, "starting".into());

    if ytp_render::kind(&project) == Kind::Song {
        // The song renderer plays the song from its own request; the MIDI file
        // belongs to the project's owner, not necessarily to whoever renders.
        let request = match ytp_render::song_request(&project) {
            Ok(r) => r,
            Err(e) => return failed(e.to_string()),
        };
        let mut as_song = rec.clone();
        as_song.owner = stored.meta.owner;
        as_song.voice = request["voice"].as_str().unwrap_or_default().to_string();
        as_song.options = json!({ "midi_id": request["midi_id"], "parts": request["parts"], "options": request["options"] });
        return crate::ytpmv::run(st, id, &as_song, quota, cancel).await;
    }

    let locator = match locator(st).await {
        Ok(l) => l,
        Err(e) => return failed(e.to_string()),
    };
    let out = st.jobs.result_path(id);
    let (st2, id2) = (st.clone(), id.to_string());
    let out2 = out.clone();
    let joined = tokio::task::spawn_blocking(move || {
        let progress = |stage: &str, done: usize, total: usize| -> bool {
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            let (p, msg) = stage_progress(stage, done, total);
            st2.jobs.set_progress(&id2, p, msg);
            true
        };
        let stop = || cancel.load(Ordering::Relaxed);
        let env = st2.projects.env(&locator);
        let opts = FullOptions { max_height: quota.max_height, encoder: Some(st2.encoder.as_ref()), cancel: Some(&stop) };
        ytp_render::render_full(&env, &project, &out2, &opts, &progress)
    })
    .await;
    match joined {
        Err(e) => failed(format!("the renderer crashed: {e}")),
        Ok(Err(ytp_render::Error::Cancelled)) => {
            Outcome { result: Err(Failure::Cancelled), report: None, options: None, warnings: Vec::new() }
        }
        Ok(Err(e)) => failed(e.to_string()),
        Ok(Ok(length)) => Outcome {
            result: Ok(Done {
                timeline: json!([]),
                duration: (length * 1000.0).round() / 1000.0,
                size_bytes: std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0),
                file: None,
            }),
            report: Some(json!({ "project_id": pid, "revision": stored.meta.revision })),
            options: None,
            warnings: Vec::new(),
        },
    }
}

// ── Preview ──────────────────────────────────────────────────────────────────

pub struct Preview {
    pub path: PathBuf,
    pub cached: bool,
    pub millis: u128,
    /// How the window was made: kind, clips played, proxies used.
    pub info: Value,
}

/// What a window shows, for the cache: the clips in it, the mixer, the
/// corpora (by version) it draws on, the window. Edits elsewhere in the
/// project do not change it.
///
/// Pass the project after [`expand_generated_clips`]: a `generated` clip still
/// in the document would hash its authored length instead of the cut it makes.
pub async fn preview_key(st: &AppState, p: &Project, owner: i64, start: f64, duration: f64) -> Result<String> {
    let mut corpora: Vec<String> = vec![p.corpus.clone()];
    let doc = match ytp_render::kind(p) {
        Kind::Song => {
            let req = ytp_render::song_request(p).map_err(|e| Error::invalid(e.to_string()))?;
            for part in req["parts"].as_array().into_iter().flatten() {
                if let Some(c) = part["corpus"].as_str() {
                    corpora.push(c.to_string());
                }
            }
            if let Some(c) = req["voice"].as_str() {
                corpora.push(c.to_string());
            }
            json!({ "song": req, "owner": owner })
        }
        Kind::Sequence => {
            let (a, b) = (start, start + duration);
            let tracks: Vec<Value> = p
                .tracks
                .iter()
                .map(|t| {
                    let clips: Vec<&ytp_core::Clip> = t.clips.iter().filter(|c| c.end() > a && c.start < b).collect();
                    for c in &clips {
                        if let ytp_core::ClipSource::CorpusClip { corpus, .. } = &c.source {
                            corpora.push(corpus.clone());
                        }
                    }
                    json!({ "id": t.id, "kind": t.kind, "mute": t.mute, "solo": t.solo, "volume": t.volume, "clips": clips })
                })
                .collect();
            json!({ "tracks": tracks, "captions": p.captions, "any_solo": p.tracks.iter().any(|t| t.solo) })
        }
    };
    corpora.sort();
    corpora.dedup();
    let versions: Vec<String> = corpora.iter().map(|c| format!("{c}:{:?}", st.corpora.version(c))).collect();
    let mut h = Sha256::new();
    h.update(format!("{PREVIEW_VERSION}|{start:.4}|{duration:.4}|{versions:?}|").as_bytes());
    h.update(doc.to_string().as_bytes());
    Ok(h.finalize().iter().take(12).map(|b| format!("{b:02x}")).collect())
}

fn touch(path: &Path) {
    if let Ok(f) = std::fs::OpenOptions::new().write(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

fn evict(dir: &Path, max_bytes: u64) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(SystemTime, u64, PathBuf)> = rd
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "mp4"))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((m.modified().ok()?, m.len(), e.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort_by_key(|f| f.0);
    for (_, len, path) in files {
        if total <= max_bytes * 9 / 10 {
            break;
        }
        if total > max_bytes && std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
        }
    }
}

/// Render `start..start + duration` of `project` at low quality; cached.
/// `owner` is the project owner's id (whose MIDI library a song plays from).
pub async fn preview(st: &AppState, project: Project, owner: i64, start: f64, duration: f64) -> Result<Preview> {
    if !(start.is_finite() && duration.is_finite()) || start < 0.0 || duration <= 0.0 {
        return Err(Error::invalid("a preview needs start >= 0 and duration > 0"));
    }
    let duration = duration.min(PREVIEW_MAX_SECONDS);
    // Expand before anything looks at the project: the window check, the cache
    // key and the renderer all have to see the clips a prompt really makes. The
    // key is hashed from the expanded clips (spans, corpora, effects, shifted
    // starts), so two seeds cannot share a cached window and the same seed
    // always hashes the same one.
    let project = expand_generated_clips(st, project).await?;
    let total = project.duration();
    if start >= total + if ytp_render::kind(&project) == Kind::Song { 0.8 } else { 0.0 } || total <= 0.0 {
        return Err(Error::invalid("the window starts after the end of the project"));
    }
    let t0 = Instant::now();
    let key = preview_key(st, &project, owner, start, duration).await?;
    let path = st.projects.preview_dir.join(format!("{key}.mp4"));
    let kind = kind_name(&project);
    let cached = |path: PathBuf| {
        touch(&path);
        Preview { path, cached: true, millis: t0.elapsed().as_millis(), info: json!({ "kind": kind }) }
    };
    if path.metadata().is_ok_and(|m| m.len() > 0) {
        return Ok(cached(path));
    }
    // A cache hit never waits for a render slot; a miss waits its turn, and
    // looks again in case the same window was rendered while it waited.
    let _permit = st.projects.previews.acquire().await.map_err(Error::internal)?;
    if path.metadata().is_ok_and(|m| m.len() > 0) {
        return Ok(cached(path));
    }
    std::fs::create_dir_all(&st.projects.preview_dir).map_err(Error::internal)?;
    let tmp = st.projects.preview_dir.join(format!("{key}.{:x}.tmp.mp4", rand::random::<u32>()));
    let info = match ytp_render::kind(&project) {
        Kind::Sequence => {
            let locator = locator(st).await?;
            let (st2, tmp2) = (st.clone(), tmp.clone());
            blocking(move || {
                let env = st2.projects.env(&locator);
                let i = ytp_render::render_preview(&env, &project, start, duration, &tmp2, Some(&st2.projects.proxy), &|| true)
                    .map_err(render_error)?;
                Ok(json!({ "kind": "sequence", "clips": i.segments, "overlays": i.overlays, "proxied": i.proxied, "duration": i.duration }))
            })
            .await
        }
        Kind::Song => song_preview(st, &project, owner, start, duration, &tmp).await,
    };
    let info = match info {
        Ok(i) => i,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };
    std::fs::rename(&tmp, &path).map_err(Error::internal)?;
    evict(&st.projects.preview_dir, PREVIEW_CACHE_BYTES);
    Ok(Preview { path, cached: false, millis: t0.elapsed().as_millis(), info })
}

fn render_error(e: ytp_render::Error) -> Error {
    match e {
        ytp_render::Error::Cancelled => Error::conflict("cancelled"),
        ytp_render::Error::Invalid(m) => Error::invalid(m),
        other => Error::internal(other),
    }
}

/// A window of a song: the mixer plays only those seconds, and only the takes
/// shown in them are decoded for the picture.
async fn song_preview(st: &AppState, project: &Project, owner: i64, start: f64, duration: f64, out: &Path) -> Result<Value> {
    let req = ytp_render::song_request(project).map_err(render_error)?;
    let midi_id = req["midi_id"].as_str().unwrap_or_default().to_string();
    let parts: Vec<Value> = req["parts"].as_array().cloned().unwrap_or_default();
    let options = mix::preview_options(&req["options"], Some(duration), None);
    let default = match req["voice"].as_str().filter(|v| !v.is_empty()) {
        Some(v) => v.to_string(),
        None => st.corpora.list().await?.into_iter().next().map(|c| c.name).unwrap_or_default(),
    };
    let song = {
        let st2 = st.clone();
        let id = midi_id.clone();
        blocking(move || st2.ytpmv.load_song(owner, &id)).await?
    };
    let mut banks = Banks::new();
    if !options.synth {
        for name in mix::corpora_of(&parts, &default) {
            if name.is_empty() || banks.contains_key(&name) {
                continue;
            }
            let pool = st.jobs.pool(st, &name).await.map_err(|_| Error::invalid(format!("The corpus '{name}' is not installed.")))?;
            banks.insert(name.clone(), st.ytpmv.bank(&name, pool));
        }
    }
    let (st2, out) = (st.clone(), out.to_path_buf());
    blocking(move || {
        let env = st2.ytpmv.env(&st2);
        let request = Request {
            song: &song,
            parts: &parts,
            options,
            default_corpus: &default,
            banks: &banks,
            env: &env,
            store: &st2.ytpmv.store,
            audition: None,
            audio_only: false,
            start_seconds: start,
            only_part: None,
            out_size: ytp_ytpmv::fit_out_size(PREVIEW_HEIGHT),
        };
        let progress = |_: &str, _: usize, _: usize| true;
        let prep = mix::prepare(&request, &progress).map_err(api_error)?;
        let mixed = mix::mix(&prep, &progress).map_err(api_error)?;
        let font = ytp_media::tools::Tools::new(&st2.projects.ffmpeg).caption_font();
        video::render_preview(&prep, &mixed, &env, font.as_deref(), &out, &progress).map_err(api_error)?;
        Ok(json!({ "kind": "song", "tiles": prep.tiles.len(), "duration": prep.t_end - prep.t_from }))
    })
    .await
}
