//! Cut-clip cache (the idea of `app/clip_cache.py`): the stretch of a source
//! video a segment reads is cut once into a small file, so a sentence rendered
//! again does not seek and decode the big source for every word.
//!
//! Content addressed: the file name is a hash of the source (path, mtime,
//! size) and the exact window, so an edited or replaced source can never
//! supply a stale cut and nothing needs invalidating. Cuts are made on a
//! background thread after a miss (the render that missed reads the source as
//! before, so the first render is not slowed); the least recently used files
//! are deleted when the directory outgrows its cap.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

use crate::proc::run;

const EXT: &str = "nut";
/// Decoded audio the planner measured (mono 16 kHz, 16-bit).
const PCM_EXT: &str = "pcm";
const QUEUE: usize = 512;
/// Bump when the cut format changes, so old files are never reused.
const FORMAT: &str = "v1";
const PCM_FORMAT: &str = "pcm1";

struct Job {
    key: String,
    source: String,
    start: f64,
    read: f64,
}

#[derive(Default)]
struct State {
    /// Bytes of finished cuts.
    total: u64,
    /// Cuts queued or being made.
    pending: HashSet<String>,
    /// Cuts that ffmpeg could not make (no audio, unreadable): not retried
    /// until restart.
    failed: HashSet<String>,
}

struct Shared {
    dir: PathBuf,
    max_bytes: u64,
    ffmpeg: PathBuf,
    state: Mutex<State>,
    idle: Condvar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheStats {
    pub files: usize,
    pub bytes: u64,
}

pub struct ClipCache {
    shared: Arc<Shared>,
    tx: SyncSender<Job>,
}

fn final_path(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.{EXT}"))
}

/// Finished cuts in `dir` as `(path, last used, bytes)`.
fn entries(dir: &Path) -> Vec<(PathBuf, SystemTime, u64)> {
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    rd.flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == EXT || x == PCM_EXT))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((e.path(), m.modified().ok()?, m.len()))
        })
        .collect()
}

/// Delete least recently used cuts until at most `max_bytes` remain; returns
/// the bytes still on disk.
pub fn trim(dir: &Path, max_bytes: u64) -> u64 {
    let mut files = entries(dir);
    let mut total: u64 = files.iter().map(|f| f.2).sum();
    if total <= max_bytes {
        return total;
    }
    files.sort_by_key(|f| f.1);
    for (path, _, size) in files {
        if total <= max_bytes {
            break;
        }
        // A file a render still has open may refuse to go (Windows); it is
        // simply kept and counted.
        if fs::remove_file(&path).is_ok() {
            total -= size;
        }
    }
    total
}

impl ClipCache {
    /// A cache in `dir` holding at most `max_bytes`, cutting with `ffmpeg`.
    pub fn new(dir: impl Into<PathBuf>, max_bytes: u64, ffmpeg: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let _ = fs::create_dir_all(&dir);
        if let Ok(rd) = fs::read_dir(&dir) {
            for e in rd.flatten() {
                // Half-made cuts of an earlier run.
                if e.path().extension().is_some_and(|x| x == "tmp") {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
        let total = trim(&dir, max_bytes);
        let shared = Arc::new(Shared {
            dir,
            max_bytes,
            ffmpeg: ffmpeg.into(),
            state: Mutex::new(State { total, ..State::default() }),
            idle: Condvar::new(),
        });
        let (tx, rx) = sync_channel::<Job>(QUEUE);
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("clip-cache".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let made = worker.make(&job);
                    let mut st = worker.state.lock().unwrap();
                    if !made {
                        st.failed.insert(job.key.clone());
                    }
                    st.pending.remove(&job.key);
                    worker.idle.notify_all();
                }
            })
            .expect("clip cache thread");
        Self { shared, tx }
    }

    /// The cut of `source` from `start` for `read` seconds if there is one;
    /// otherwise `None`, and one is made in the background for next time.
    pub fn lookup(&self, source: &str, start: f64, read: f64) -> Option<PathBuf> {
        if !start.is_finite() || start < 0.0 || !read.is_finite() || read <= 0.0 {
            return None;
        }
        let key = key_for(source, start, read)?;
        let path = final_path(&self.shared.dir, &key);
        if path.is_file() {
            // Least recently used is judged by modification time.
            if let Ok(f) = fs::OpenOptions::new().write(true).open(&path) {
                let _ = f.set_modified(SystemTime::now());
            }
            return Some(path);
        }
        let mut st = self.shared.state.lock().unwrap();
        if !st.failed.contains(&key) && st.pending.insert(key.clone()) {
            let job = Job { key: key.clone(), source: source.to_string(), start, read };
            if let Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) = self.tx.try_send(job) {
                st.pending.remove(&key);
            }
        }
        None
    }

    /// Block until every queued cut is done (tests, benchmarks).
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let st = self.shared.state.lock().unwrap();
        let (st, res) = self
            .shared
            .idle
            .wait_timeout_while(st, timeout, |s| !s.pending.is_empty())
            .unwrap();
        drop(st);
        !res.timed_out()
    }

    /// Samples of source from start for duration seconds as an earlier
    /// decode stored them, exactly (16-bit mono 16 kHz, scaled to -1..1).
    pub fn audio(&self, source: &str, start: f64, duration: f64) -> Option<Vec<f32>> {
        let key = key_tagged(PCM_FORMAT, source, start, duration)?;
        let path = self.shared.dir.join(format!("{key}.{PCM_EXT}"));
        let bytes = fs::read(&path).ok()?;
        if let Ok(f) = fs::OpenOptions::new().write(true).open(&path) {
            let _ = f.set_modified(SystemTime::now());
        }
        Some(
            bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_le_bytes(*b) as f32 / 32768.0)
                .collect(),
        )
    }

    /// Keep a decode for next time. `raw` is the little-endian 16-bit PCM
    /// ffmpeg wrote.
    pub fn put_audio(&self, source: &str, start: f64, duration: f64, raw: &[u8]) {
        let Some(key) = key_tagged(PCM_FORMAT, source, start, duration) else { return };
        let path = self.shared.dir.join(format!("{key}.{PCM_EXT}"));
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = self.shared.dir.join(format!("{key}.{n}.tmp"));
        if fs::write(&tmp, raw).is_err() || fs::rename(&tmp, &path).is_err() {
            let _ = fs::remove_file(&tmp);
            return;
        }
        self.shared.added(raw.len() as u64);
    }

    pub fn stats(&self) -> CacheStats {
        let e = entries(&self.shared.dir);
        CacheStats { files: e.len(), bytes: e.iter().map(|f| f.2).sum() }
    }

    pub fn max_bytes(&self) -> u64 {
        self.shared.max_bytes
    }

    /// Delete every cut.
    pub fn clear(&self) {
        for (p, _, _) in entries(&self.shared.dir) {
            let _ = fs::remove_file(p);
        }
        self.shared.state.lock().unwrap().total = trim(&self.shared.dir, u64::MAX);
    }
}

impl Shared {
    /// Cut one window to a temporary file, move it into place, and trim.
    fn make(&self, job: &Job) -> bool {
        let path = final_path(&self.dir, &job.key);
        if path.is_file() {
            return true;
        }
        let tmp = self.dir.join(format!("{}.tmp", job.key));
        let mut cmd = std::process::Command::new(&self.ffmpeg);
        cmd.args(["-y", "-hide_banner", "-loglevel", "error", "-nostdin"])
            .args(["-ss", &format!("{:.4}", job.start), "-t", &format!("{:.4}", job.read)])
            .args(["-i", &job.source])
            .args(["-map", "0:v:0", "-map", "0:a:0"])
            // Near-lossless: it is encoded again, to crf 23, at the end.
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-crf", "14", "-pix_fmt", "yuv420p"])
            .args(["-c:a", "pcm_s16le", "-f", "nut"])
            .arg(&tmp);
        let ok = matches!(run(&mut cmd, Duration::from_secs(120), &|| true), Ok(o) if o.success);
        if !ok || fs::rename(&tmp, &path).is_err() {
            let _ = fs::remove_file(&tmp);
            return false;
        }
        self.added(fs::metadata(&path).map(|m| m.len()).unwrap_or(0));
        true
    }

    /// Count a new file, and trim if the directory has outgrown its cap.
    fn added(&self, size: u64) {
        let total = {
            let mut st = self.state.lock().unwrap();
            st.total += size;
            st.total
        };
        if total > self.max_bytes {
            let left = trim(&self.dir, self.max_bytes);
            self.state.lock().unwrap().total = left;
        }
    }
}

/// The content address of a window: the source as it is on disk now, and the
/// window to the precision ffmpeg is given.
pub fn key_for(source: &str, start: f64, read: f64) -> Option<String> {
    key_tagged(FORMAT, source, start, read)
}

fn key_tagged(tag: &str, source: &str, start: f64, read: f64) -> Option<String> {
    let meta = fs::metadata(source).ok()?;
    let real = fs::canonicalize(source).unwrap_or_else(|_| PathBuf::from(source));
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut h = Sha256::new();
    h.update(format!(
        "{tag}\0{}\0{mtime}\0{}\0{start:.4}\0{read:.4}",
        real.to_string_lossy(),
        meta.len()
    ));
    let digest = h.finalize();
    Some(digest[..12].iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(dir: &Path, name: &str, bytes: usize, age_secs: u64) {
        let p = dir.join(name);
        fs::write(&p, vec![0u8; bytes]).unwrap();
        let f = fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(age_secs)).unwrap();
    }

    #[test]
    fn trimming_deletes_the_least_recently_used_first() {
        let d = tempfile::tempdir().unwrap();
        put(d.path(), "old.nut", 400, 300);
        put(d.path(), "mid.nut", 400, 200);
        put(d.path(), "new.nut", 400, 100);
        put(d.path(), "notes.txt", 400, 1000); // not ours: never counted or removed
        assert_eq!(trim(d.path(), 1200), 1200);
        assert!(d.path().join("old.nut").exists());
        assert_eq!(trim(d.path(), 900), 800);
        assert!(!d.path().join("old.nut").exists());
        assert!(d.path().join("mid.nut").exists() && d.path().join("new.nut").exists());
        assert_eq!(trim(d.path(), 0), 0);
        assert!(d.path().join("notes.txt").exists());
    }

    #[test]
    fn keys_follow_the_source_and_the_window() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("a.mp4");
        fs::write(&src, b"one").unwrap();
        let s = src.to_str().unwrap();
        let k = key_for(s, 1.0, 0.5).unwrap();
        assert_eq!(k, key_for(s, 1.0, 0.5).unwrap());
        assert_eq!(k, key_for(s, 1.00001, 0.5).unwrap(), "below the 0.1 ms ffmpeg is given");
        assert_ne!(k, key_for(s, 1.0001, 0.5).unwrap());
        assert_ne!(k, key_for(s, 1.0, 0.6).unwrap());
        // a replaced source never reuses the old cut
        fs::write(&src, b"another one").unwrap();
        assert_ne!(k, key_for(s, 1.0, 0.5).unwrap());
        assert!(key_for("no/such/file.mp4", 1.0, 0.5).is_none());
    }

    #[test]
    fn decoded_audio_comes_back_exactly_and_counts_against_the_cap() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("a.mp4");
        fs::write(&src, b"x").unwrap();
        let s = src.to_str().unwrap();
        let cache_dir = d.path().join("cache");
        let c = ClipCache::new(&cache_dir, 1000, "ffmpeg-not-needed");
        assert!(c.audio(s, 1.0, 0.5).is_none());
        let raw: Vec<u8> = [0i16, 1, -1, 32767, -32768].iter().flat_map(|v| v.to_le_bytes()).collect();
        c.put_audio(s, 1.0, 0.5, &raw);
        let got = c.audio(s, 1.0, 0.5).unwrap();
        assert_eq!(got, vec![0.0, 1.0 / 32768.0, -1.0 / 32768.0, 32767.0 / 32768.0, -1.0]);
        assert!(c.audio(s, 1.0, 0.6).is_none());
        // 20 more of 400 bytes: the oldest go, the cap holds
        for k in 0..20 {
            c.put_audio(s, 2.0 + k as f64, 0.5, &[0u8; 400]);
        }
        assert!(c.stats().bytes <= 1000, "{:?}", c.stats());
    }

    #[test]
    fn startup_removes_half_made_cuts_and_trims() {
        let d = tempfile::tempdir().unwrap();
        put(d.path(), "dead.tmp", 10, 5);
        put(d.path(), "a.nut", 500, 50);
        put(d.path(), "b.nut", 500, 10);
        let c = ClipCache::new(d.path(), 600, "ffmpeg-not-needed");
        assert!(!d.path().join("dead.tmp").exists());
        assert_eq!(c.stats(), CacheStats { files: 1, bytes: 500 });
        assert!(d.path().join("b.nut").exists());
    }
}
