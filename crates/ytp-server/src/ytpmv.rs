//! YTPMV on the server: each user's MIDI library, the corpus banks the
//! instruments are cut from, and the render job (song → video).
//!
//! MIDI files are kept under `<data>/midi/<user id>/<id>.mid`, beside a
//! `.title` and a cached `.info.json` description. The id is a hash of the
//! file, so uploading the same song twice is one file. Everything here is
//! plain blocking code; handlers call it through `spawn_blocking`.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use ytp_auth::Quota;
use ytp_core::{Error, Result};
use ytp_music::music::{self, Song};
use ytp_splice::Pool;
use ytp_ytpmv::mix::{self, Request};
use ytp_ytpmv::recommend::AnalysisStore;
use ytp_ytpmv::samples::{Bank, Banks, Env};
use ytp_ytpmv::settings::Options;
use ytp_ytpmv::{video, MAX_SECONDS};

use crate::jobs::{Done, Failure, JobRecord, Outcome};
use crate::AppState;

pub const MAX_MIDI_BYTES: usize = 2 * 1024 * 1024;
/// Most MIDI files one user keeps.
pub const MAX_LIBRARY: usize = 200;
/// Songs kept parsed.
const SONG_CACHE: usize = 6;

type SongKey = (i64, String, u64, Option<SystemTime>, String);

pub struct Service {
    midi_dir: PathBuf,
    samples_dir: PathBuf,
    pub ffmpeg: PathBuf,
    pub store: AnalysisStore,
    songs: Mutex<VecDeque<(SongKey, Arc<Song>)>>,
    banks: Mutex<HashMap<String, (usize, Arc<Bank>)>>,
}

fn io(e: std::io::Error) -> Error {
    Error::internal(e)
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 12 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn modified(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

impl Service {
    pub fn new(data_dir: &Path, ffmpeg: PathBuf) -> Self {
        let root = data_dir.join("ytpmv");
        Self {
            midi_dir: data_dir.join("midi"),
            samples_dir: root.join("samples"),
            store: AnalysisStore::new(root),
            ffmpeg,
            songs: Mutex::new(VecDeque::new()),
            banks: Mutex::new(HashMap::new()),
        }
    }

    fn user_dir(&self, owner: i64) -> PathBuf {
        self.midi_dir.join(owner.to_string())
    }

    fn path(&self, owner: i64, id: &str) -> Result<PathBuf> {
        if !valid_id(id) {
            return Err(Error::not_found("No such MIDI file."));
        }
        Ok(self.user_dir(owner).join(format!("{id}.mid")))
    }

    pub fn env<'a>(&'a self, st: &'a AppState) -> Env<'a> {
        Env { ffmpeg: &self.ffmpeg, renderer: st.renderer.as_ref(), samples_dir: &self.samples_dir }
    }

    /// The bank for corpus `name`, built once per version of its pool.
    pub fn bank(&self, name: &str, pool: Arc<Pool>) -> Arc<Bank> {
        let ptr = Arc::as_ptr(&pool) as usize;
        let mut g = self.banks.lock().unwrap();
        if let Some((p, b)) = g.get(name) {
            if *p == ptr {
                return b.clone();
            }
            self.store.forget(name);
        }
        let bank = Arc::new(Bank::new(name, pool));
        g.insert(name.to_string(), (ptr, bank.clone()));
        bank
    }

    // ── The library ──────────────────────────────────────────────────────────

    /// Store a MIDI file for `owner`; the id of the file.
    pub fn save(&self, owner: i64, data: &[u8], filename: &str) -> Result<String> {
        if data.is_empty() {
            return Err(Error::invalid("That file is empty."));
        }
        if data.len() > MAX_MIDI_BYTES {
            return Err(Error::invalid("That MIDI file is too big (2 MB is the limit)."));
        }
        if !data.starts_with(b"MThd") {
            return Err(Error::invalid("That doesn't look like a MIDI file."));
        }
        let title: String = Path::new(filename)
            .file_stem()
            .map(|s| s.to_string_lossy().chars().filter(|c| !c.is_control()).take(80).collect())
            .unwrap_or_default();
        let song = music::parse(data, title.trim())
            .map_err(|e| Error::invalid(format!("Couldn't read that MIDI file: {e}")))?;
        if song.parts.is_empty() {
            return Err(Error::invalid("That MIDI file has no notes in it."));
        }
        let mut h = Sha256::new();
        h.update(data);
        let id: String = h.finalize().iter().take(6).map(|b| format!("{b:02x}")).collect();
        let dir = self.user_dir(owner);
        std::fs::create_dir_all(&dir).map_err(io)?;
        let path = dir.join(format!("{id}.mid"));
        if !path.exists() {
            let kept = std::fs::read_dir(&dir)
                .map(|rd| rd.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "mid")).count())
                .unwrap_or(0);
            if kept >= MAX_LIBRARY {
                return Err(Error::conflict(format!("Your MIDI library is full ({MAX_LIBRARY} files); delete some first.")));
            }
            std::fs::write(&path, data).map_err(io)?;
        }
        let title = title.trim();
        if !title.is_empty() {
            std::fs::write(sibling(&path, "title"), title).map_err(io)?;
            let _ = std::fs::remove_file(sibling(&path, "info.json"));
        }
        Ok(id)
    }

    pub fn load_song(&self, owner: i64, id: &str) -> Result<Arc<Song>> {
        let path = self.path(owner, id)?;
        let data = std::fs::read(&path).map_err(|_| Error::not_found("That MIDI file has gone — upload it again."))?;
        let title = std::fs::read_to_string(sibling(&path, "title")).map(|t| t.trim().to_string()).unwrap_or_default();
        // One render asks for the song three times (describe, recommend,
        // render) and a big file takes a while to parse; nothing changes a
        // song once it is parsed, so the same one is handed out while the
        // file is.
        let key: SongKey = (owner, id.to_string(), data.len() as u64, modified(&path), title.clone());
        {
            let mut c = self.songs.lock().unwrap();
            if let Some(i) = c.iter().position(|(k, _)| *k == key) {
                let e = c.remove(i).unwrap();
                let s = e.1.clone();
                c.push_back(e);
                return Ok(s);
            }
        }
        let song = music::parse(&data, &title).map_err(|e| Error::invalid(format!("Couldn't read that MIDI file: {e}")))?;
        if song.parts.is_empty() {
            return Err(Error::invalid("That MIDI file has no notes in it."));
        }
        let song = Arc::new(song);
        let mut c = self.songs.lock().unwrap();
        c.push_back((key, song.clone()));
        while c.len() > SONG_CACHE {
            c.pop_front();
        }
        Ok(song)
    }

    /// Every MIDI file `owner` keeps, newest first, each described.
    ///
    /// The description is cached beside the file, because a big arrangement
    /// takes a second or two to read and the list is read every time the page
    /// opens.
    pub fn library(&self, owner: i64) -> Vec<Value> {
        let Ok(rd) = std::fs::read_dir(self.user_dir(owner)) else { return Vec::new() };
        let mut out = Vec::new();
        for e in rd.flatten() {
            let path = e.path();
            let Some(id) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else { continue };
            if path.extension().is_none_or(|x| x != "mid") || !valid_id(&id) {
                continue;
            }
            let cache = sibling(&path, "info.json");
            let newest = [modified(&path), modified(&sibling(&path, "title"))].into_iter().flatten().max();
            let fresh = matches!((modified(&cache), newest), (Some(c), Some(n)) if c >= n);
            let mut info: Option<Value> =
                if fresh { std::fs::read(&cache).ok().and_then(|b| serde_json::from_slice(&b).ok()) } else { None };
            if info.is_none() {
                let Ok(song) = self.load_song(owner, &id) else { continue };
                let v = ytp_ytpmv::song::library_info(&song);
                if let Ok(b) = serde_json::to_vec(&v) {
                    let _ = std::fs::write(&cache, b);
                }
                info = Some(v);
            }
            let mut v = info.unwrap();
            v["midi_id"] = json!(id);
            v["bytes"] = json!(std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0));
            let added = modified(&path)
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            v["added"] = json!(added);
            out.push(v);
        }
        out.sort_by_key(|v| std::cmp::Reverse(v["added"].as_u64().unwrap_or(0)));
        out
    }

    pub fn rename(&self, owner: i64, id: &str, title: &str) -> Result<Value> {
        let path = self.path(owner, id)?;
        if !path.is_file() {
            return Err(Error::not_found("That MIDI file has gone — upload it again."));
        }
        let title = title.trim();
        if title.is_empty() || title.chars().count() > 80 || title.chars().any(|c| (c as u32) < 32) {
            return Err(Error::invalid("Use a MIDI title of 1–80 characters on one line."));
        }
        std::fs::write(sibling(&path, "title"), title).map_err(io)?;
        let _ = std::fs::remove_file(sibling(&path, "info.json"));
        Ok(json!({ "midi_id": id, "title": title }))
    }

    pub fn delete(&self, owner: i64, id: &str) -> Result<()> {
        let path = self.path(owner, id)?;
        std::fs::remove_file(&path).map_err(|_| Error::not_found("That MIDI file has gone — upload it again."))?;
        let _ = std::fs::remove_file(sibling(&path, "title"));
        let _ = std::fs::remove_file(sibling(&path, "info.json"));
        self.songs.lock().unwrap().retain(|(k, _)| !(k.0 == owner && k.1 == id));
        Ok(())
    }
}

/// `<file>.mid` → `<file>.mid.<ext>`
fn sibling(path: &Path, ext: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".");
    s.push(ext);
    PathBuf::from(s)
}

/// The error a failed ytpmv step is reported as.
pub fn api_error(e: ytp_ytpmv::Error) -> Error {
    match e {
        ytp_ytpmv::Error::Cancelled => Error::conflict("cancelled"),
        ytp_ytpmv::Error::Msg(m) => Error::invalid(m),
        ytp_ytpmv::Error::NotFound(m) => Error::not_found(m),
        ytp_ytpmv::Error::Internal(m) => Error::internal(m),
    }
}

/// A JSON value as ASCII, so it can travel in a header.
pub fn ascii_json(v: &Value) -> String {
    let mut out = String::new();
    for c in v.to_string().chars() {
        if c.is_ascii() && !c.is_control() {
            out.push(c);
        } else {
            let mut buf = [0u16; 2];
            for u in c.encode_utf16(&mut buf) {
                out.push_str(&format!("\\u{u:04x}"));
            }
        }
    }
    out
}

// ── The render job ───────────────────────────────────────────────────────────

/// Where a stage of a render sits on the job's 0–1 bar.
fn stage_progress(stage: &str, done: usize, total: usize) -> (f32, String) {
    let frac = if total == 0 { 0.0 } else { (done as f32 / total as f32).min(1.0) };
    match stage {
        "measuring" => (0.08 * frac, format!("measuring words {done}/{total}")),
        "samples" => (0.08 + 0.04 * frac, "choosing sounds".into()),
        "notes" => (0.12 + 0.23 * frac, format!("playing notes {done}/{total}")),
        "tiles" => (0.35 + 0.05 * frac, format!("loading clips {done}/{total}")),
        "frames" => (0.40 + 0.50 * frac, format!("drawing frames {done}/{total}")),
        "encoding" => (0.90 + 0.09 * frac, "encoding".into()),
        other => (0.0, other.to_string()),
    }
}

fn failed(msg: impl Into<String>) -> Outcome {
    Outcome { result: Err(Failure::Failed(msg.into())), report: None, options: None, warnings: Vec::new() }
}

/// Run a queued render: the MIDI file played by the chosen voices, to a video.
pub(crate) async fn run(st: &AppState, id: &str, rec: &JobRecord, quota: Quota, cancel: Arc<AtomicBool>) -> Outcome {
    let midi_id = rec.options["midi_id"].as_str().unwrap_or_default().to_string();
    let parts: Vec<Value> = rec.options["parts"].as_array().cloned().unwrap_or_default();
    let options = rec.options["options"].clone();
    let default_corpus = rec.voice.clone();
    let owner = rec.owner;

    let svc = &st.ytpmv;
    let song = {
        let st2 = st.clone();
        let mid = midi_id.clone();
        match tokio::task::spawn_blocking(move || st2.ytpmv.load_song(owner, &mid)).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return failed(e.to_string()),
            Err(e) => return failed(format!("the renderer crashed: {e}")),
        }
    };
    let opts = Options::from_json(&options, MAX_SECONDS);
    let mut banks: Banks = HashMap::new();
    if !opts.synth {
        for name in mix::corpora_of(&parts, &default_corpus) {
            if name.is_empty() || banks.contains_key(&name) {
                continue;
            }
            match st.jobs.pool(st, &name).await {
                Ok(pool) => {
                    banks.insert(name.clone(), svc.bank(&name, pool));
                }
                Err(e) => return failed(format!("voice {name:?}: {e}")),
            }
        }
    }

    let st2 = st.clone();
    let id2 = id.to_string();
    let out_path = st.jobs.result_path(id);
    let joined = tokio::task::spawn_blocking(move || {
        let progress = |stage: &str, done: usize, total: usize| -> bool {
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            let (p, msg) = stage_progress(stage, done, total);
            st2.jobs.set_progress(&id2, p, msg);
            true
        };
        render(&st2, &song, &parts, opts, &default_corpus, &banks, quota, &out_path, &progress)
    })
    .await;
    match joined {
        Ok(o) => o,
        Err(e) => failed(format!("the renderer crashed: {e}")),
    }
}

#[allow(clippy::too_many_arguments)]
fn render(
    st: &AppState,
    song: &Song,
    parts: &[Value],
    opts: Options,
    default_corpus: &str,
    banks: &Banks,
    quota: Quota,
    out: &Path,
    progress: &(dyn Fn(&str, usize, usize) -> bool + Sync),
) -> Outcome {
    let svc = &st.ytpmv;
    let env = svc.env(st);
    let cancelled = || Outcome { result: Err(Failure::Cancelled), report: None, options: None, warnings: Vec::new() };
    let fail = |e: ytp_ytpmv::Error| match e {
        ytp_ytpmv::Error::Cancelled => cancelled(),
        other => failed(other.to_string()),
    };
    let req = Request {
        song,
        parts,
        options: opts,
        default_corpus,
        banks,
        env: &env,
        store: &svc.store,
        audition: None,
        audio_only: false,
        start_seconds: 0.0,
        only_part: None,
    };
    let prep = match mix::prepare(&req, progress) {
        Ok(p) => p,
        Err(e) => return fail(e),
    };
    let duration = prep.t_end - prep.t_from;
    if duration > quota.max_render_seconds as f64 {
        return failed(format!("the video would be {duration:.1} s long; your limit is {} s", quota.max_render_seconds));
    }
    let mixed = match mix::mix(&prep, progress) {
        Ok(m) => m,
        Err(e) => return fail(e),
    };
    let font = ytp_media::tools::Tools::new(&svc.ffmpeg).caption_font();
    if let Err(e) = video::render_video(&prep, &mixed, &env, font.as_deref(), out, progress) {
        return fail(e);
    }
    let mut warnings = Vec::new();
    for p in &prep.problems {
        warnings.push(format!(
            "{}: {}",
            p["name"].as_str().unwrap_or("a part"),
            p["error"].as_str().filter(|e| !e.is_empty()).unwrap_or("no sound could be made")
        ));
    }
    let report = mix::render_summary(&prep, &mixed);
    Outcome {
        result: Ok(Done {
            timeline: json!([]),
            duration: (duration * 1000.0).round() / 1000.0,
            size_bytes: std::fs::metadata(out).map(|m| m.len()).unwrap_or(0),
        }),
        report: Some(report),
        options: None,
        warnings,
    }
}
