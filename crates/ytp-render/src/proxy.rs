//! Preview proxies: an all-intra 480p copy of a source video (the
//! `preview-proxy` preset), made in the background the first time a preview
//! plays the source and kept in a size-capped disk cache.
//!
//! A preview never waits for a proxy. The first one reads the original, which
//! is what keeps it quick to start; once the proxy exists later previews read
//! it, and with every frame a keyframe, jumping to a clip costs no decoding of
//! what came before it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

/// Sources longer than this are not proxied: an hour of all-intra video is
/// gigabytes, and a long source is also the one a preview seeks well in.
pub const MAX_SOURCE_SECONDS: f64 = 1800.0;
/// Sources this tall or shorter are not proxied.
pub const MAX_PROXY_SKIP_HEIGHT: u32 = 480;
/// Proxies being made at once.
const MAX_BUILDS: usize = 2;

pub struct ProxyCache {
    dir: PathBuf,
    max_bytes: u64,
    encoder: Arc<ytp_encode::Encoder>,
    building: Mutex<HashSet<PathBuf>>,
    running: AtomicUsize,
}

impl ProxyCache {
    pub fn new(dir: impl Into<PathBuf>, max_bytes: u64, encoder: Arc<ytp_encode::Encoder>) -> Arc<Self> {
        let dir = dir.into();
        // Left by an interrupted build.
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                if e.path().to_string_lossy().ends_with(".tmp.mp4") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        Arc::new(Self { dir, max_bytes, encoder, building: Mutex::new(HashSet::new()), running: AtomicUsize::new(0) })
    }

    fn key(source: &Path) -> Option<String> {
        let meta = std::fs::metadata(source).ok()?;
        let mut h = Sha256::new();
        h.update(source.to_string_lossy().as_bytes());
        h.update(meta.len().to_le_bytes());
        if let Some(t) = meta.modified().ok().and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()) {
            h.update(t.as_secs().to_le_bytes());
        }
        Some(h.finalize().iter().take(10).map(|b| format!("{b:02x}")).collect())
    }

    /// The proxy of `source` when it is ready; otherwise `None`, and one is
    /// started if there is room to.
    pub fn lookup(self: &Arc<Self>, source: &str) -> Option<String> {
        let source = Path::new(source);
        let key = Self::key(source)?;
        let path = self.dir.join(format!("{key}.mp4"));
        if path.metadata().is_ok_and(|m| m.len() > 0) {
            if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&path) {
                let _ = f.set_modified(SystemTime::now());
            }
            return Some(path.to_string_lossy().into_owned());
        }
        if self.dir.join(format!("{key}.skip")).exists() {
            return None;
        }
        self.schedule(source.to_path_buf(), key);
        None
    }

    fn schedule(self: &Arc<Self>, source: PathBuf, key: String) {
        if self.running.load(Ordering::Relaxed) >= MAX_BUILDS {
            return;
        }
        if !self.building.lock().unwrap().insert(source.clone()) {
            return;
        }
        self.running.fetch_add(1, Ordering::Relaxed);
        let me = self.clone();
        std::thread::spawn(move || {
            me.build(&source, &key);
            me.building.lock().unwrap().remove(&source);
            me.running.fetch_sub(1, Ordering::Relaxed);
        });
    }

    fn build(&self, source: &Path, key: &str) {
        let _ = std::fs::create_dir_all(&self.dir);
        let skip = || {
            let _ = std::fs::write(self.dir.join(format!("{key}.skip")), b"");
        };
        match self.encoder.probe(source) {
            // A source already at preview size gains nothing from a copy: the proxy
            // pays for big pictures (and long GOPs), not for 480p.
            Ok(p) if p.has_video && p.duration > 0.0 && p.duration <= MAX_SOURCE_SECONDS && p.height > MAX_PROXY_SKIP_HEIGHT => {}
            _ => return skip(),
        }
        let Some(preset) = self.encoder.presets().get("preview-proxy") else { return };
        let tmp = self.dir.join(format!("{key}.tmp.mp4"));
        let out = self.dir.join(format!("{key}.mp4"));
        match self.encoder.encode(source, preset, ytp_encode::Overrides::default(), &tmp, &|_| {}, &|| false) {
            Ok(_) => {
                if std::fs::rename(&tmp, &out).is_err() {
                    let _ = std::fs::remove_file(&tmp);
                }
                self.evict();
            }
            Err(e) => {
                tracing::debug!(source = %source.display(), error = %e, "preview proxy not made");
                let _ = std::fs::remove_file(&tmp);
                skip();
            }
        }
    }

    /// Delete the least recently used proxies until the cache fits.
    fn evict(&self) {
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return };
        let mut files: Vec<(SystemTime, u64, PathBuf)> = rd
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "mp4"))
            .filter_map(|e| {
                let m = e.metadata().ok()?;
                Some((m.modified().ok()?, m.len(), e.path()))
            })
            .collect();
        let mut total: u64 = files.iter().map(|f| f.1).sum();
        if total <= self.max_bytes {
            return;
        }
        files.sort_by_key(|f| f.0);
        for (_, len, path) in files {
            if total <= self.max_bytes * 9 / 10 {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(len);
            }
        }
    }

    /// Bytes the cache holds.
    pub fn size(&self) -> u64 {
        std::fs::read_dir(&self.dir)
            .map(|rd| rd.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "mp4")).filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum())
            .unwrap_or(0)
    }

    /// Wait for proxies being made (tests and benchmarks).
    pub fn wait_idle(&self, timeout: Duration) {
        let end = std::time::Instant::now() + timeout;
        while self.running.load(Ordering::Relaxed) > 0 && std::time::Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
