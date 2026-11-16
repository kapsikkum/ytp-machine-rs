//! Uploaded media assets: one private store per user under
//! `<data>/media/<owner>/<asset>.<ext>`, with ffprobe's reading of the file in a
//! sidecar beside it (`<asset>.json`, the same style as the jobs' sidecars).
//!
//! Only these files are ever played by the renderer, and the renderer looks
//! inside the *project owner's* directory only (see
//! `ytp_render::sequence::Env::media_path`), so a project cannot reach another
//! user's uploads by naming their asset id.
//!
//! Nothing expires on its own: an asset lives until it is deleted. Listing
//! reports what is on disk, including a file whose sidecar never got written
//! (a crash between the two writes): that is the "orphan" report, and it never
//! deletes anything.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rand::Rng;
use serde::{Deserialize, Serialize};
use ytp_core::time::{now, rfc3339};
use ytp_core::{Error, Result};
use ytp_encode::{Encoder, Probe};

/// Largest file one upload may store, bytes.
pub const MAX_MEDIA_BYTES: usize = 200 << 20;

/// Video containers an upload may have.
pub const VIDEO_EXTS: &[&str] = &["mp4", "mov", "webm", "mkv"];
/// Audio containers an upload may have.
pub const AUDIO_EXTS: &[&str] = &["wav", "mp3", "m4a", "aac", "flac", "ogg"];
/// Still images an upload may have.
pub const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

/// What a file is, by its extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Video,
    Audio,
    Image,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Video => "video",
            Kind::Audio => "audio",
            Kind::Image => "image",
        }
    }
}

/// The kind of a lower-case extension, or `None` when it is not accepted.
pub fn kind_of(ext: &str) -> Option<Kind> {
    if VIDEO_EXTS.contains(&ext) {
        Some(Kind::Video)
    } else if AUDIO_EXTS.contains(&ext) {
        Some(Kind::Audio)
    } else if IMAGE_EXTS.contains(&ext) {
        Some(Kind::Image)
    } else {
        None
    }
}

/// Every extension an asset may have, for error messages and for the renderer.
pub fn allowed_exts() -> Vec<&'static str> {
    let mut v = Vec::new();
    v.extend_from_slice(VIDEO_EXTS);
    v.extend_from_slice(AUDIO_EXTS);
    v.extend_from_slice(IMAGE_EXTS);
    v
}

/// The asset id: 1-64 of `A-Za-z0-9_-` (what a project document may name).
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Longest display name kept, characters.
const MAX_NAME_CHARS: usize = 200;

/// One stored asset: the file's ffprobe reading, kept in `<id>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub id: String,
    pub name: String,
    pub ext: String,
    pub kind: Kind,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_codec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_codec: Option<String>,
    pub uploaded_at: String,
}

/// An asset as the API answers with it: the stored fields plus how many stored
/// projects mention its id.
#[derive(Debug, Clone, Serialize)]
pub struct Listed {
    #[serde(flatten)]
    pub asset: Asset,
    pub referenced_by: i64,
}

/// Bytes accepted for one upload, counted chunk by chunk so a large body is
/// never held in RAM and a file that passes the cap is not written at all.
#[derive(Debug)]
pub struct Meter {
    cap: usize,
    size: usize,
}

impl Meter {
    pub fn new(cap: usize) -> Self {
        Self { cap, size: 0 }
    }

    /// Count `n` more bytes; over the cap is refused.
    pub fn add(&mut self, n: usize) -> Result<()> {
        self.size += n;
        if self.size > self.cap {
            return Err(Error::TooLarge(format!(
                "the file is over the {} MB limit",
                self.cap >> 20
            )));
        }
        Ok(())
    }

    pub fn size(&self) -> usize {
        self.size
    }
}

fn clean_name(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

/// The media store. Only a path: it is cheap to make per request.
pub struct Service {
    root: PathBuf,
}

impl Service {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            root: data_dir.join("media"),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where `owner`'s assets live.
    pub fn owner_dir(&self, owner: i64) -> PathBuf {
        self.root.join(owner.to_string())
    }

    /// Where an upload is spooled while it arrives (on the same volume, so the
    /// finished file is moved into place with a rename). The root is made here:
    /// the spool file is written before [`Service::install`] ever runs.
    pub fn upload_path(&self) -> PathBuf {
        let _ = std::fs::create_dir_all(&self.root);
        let n: u64 = rand::thread_rng().gen();
        self.root
            .join(format!("upload-{}-{n:016x}.part", std::process::id()))
    }

    fn file(&self, owner: i64, id: &str, ext: &str) -> PathBuf {
        self.owner_dir(owner).join(format!("{id}.{ext}"))
    }

    fn sidecar(&self, owner: i64, id: &str) -> PathBuf {
        self.owner_dir(owner).join(format!("{id}.json"))
    }

    /// The asset with `id`, if `owner` has a file of an accepted type for it.
    pub fn get(&self, owner: i64, id: &str) -> Option<Asset> {
        if !valid_id(id) {
            return None;
        }
        let dir = self.owner_dir(owner);
        let sidecar = self.sidecar(owner, id);
        if let Ok(bytes) = std::fs::read(&sidecar) {
            if let Ok(asset) = serde_json::from_slice::<Asset>(&bytes) {
                if is_regular_file(&self.file(owner, id, &asset.ext)) {
                    return Some(asset);
                }
                return None;
            }
        }
        // No sidecar: report the file itself rather than hide it (an orphan
        // candidate; nothing is removed).
        let rd = std::fs::read_dir(&dir).ok()?;
        for e in rd.flatten() {
            let path = e.path();
            let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some((stem, raw_ext)) = file_name.rsplit_once('.') else {
                continue;
            };
            let ext = raw_ext.to_lowercase();
            let Some(kind) = kind_of(&ext) else { continue };
            if stem != id || !is_regular_file(&path) {
                continue;
            }
            let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            return Some(Asset {
                id: id.to_string(),
                name: file_name.to_string(),
                ext,
                kind,
                bytes,
                duration: None,
                width: None,
                height: None,
                video_codec: None,
                audio_codec: None,
                uploaded_at: modified(&path),
            });
        }
        None
    }

    /// Every asset `owner` has, newest first.
    pub fn list(&self, owner: i64) -> Vec<Asset> {
        let Ok(rd) = std::fs::read_dir(self.owner_dir(owner)) else {
            return Vec::new();
        };
        let mut ids: HashSet<String> = HashSet::new();
        for e in rd.flatten() {
            let path = e.path();
            let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some((id, ext)) = file_name.rsplit_once('.') else {
                continue;
            };
            let ext = ext.to_lowercase();
            if kind_of(&ext).is_none() || !valid_id(id) {
                continue;
            }
            if path
                .symlink_metadata()
                .map(|m| m.file_type().is_file())
                .unwrap_or(false)
            {
                ids.insert(id.to_string());
            }
        }
        let mut assets: Vec<Asset> = ids.iter().filter_map(|id| self.get(owner, id)).collect();
        assets.sort_by(|a, b| b.uploaded_at.cmp(&a.uploaded_at).then_with(|| a.id.cmp(&b.id)));
        assets
    }

    /// Every asset of every user (an admin's view), newest first.
    pub fn list_all(&self) -> Vec<Asset> {
        let Ok(rd) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut owners: Vec<i64> = rd
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().to_str()?.parse::<i64>().ok())
            .collect();
        owners.sort_unstable();
        let mut all: Vec<Asset> = owners.into_iter().flat_map(|o| self.list(o)).collect();
        all.sort_by(|a, b| b.uploaded_at.cmp(&a.uploaded_at).then_with(|| a.id.cmp(&b.id)));
        all
    }

    /// The asset with `id` in any user's directory, with its owner (an admin
    /// deleting somebody else's file).
    pub fn find_any(&self, id: &str) -> Option<(i64, Asset)> {
        let rd = std::fs::read_dir(&self.root).ok()?;
        for e in rd.flatten() {
            let Some(owner) = e.file_name().to_str().and_then(|n| n.parse::<i64>().ok()) else {
                continue;
            };
            if let Some(asset) = self.get(owner, id) {
                return Some((owner, asset));
            }
        }
        None
    }

    /// Validate `spool` with ffprobe and store it as a new asset of `owner`.
    /// `filename` is the name the upload carried (its extension decides the
    /// kind); `name` the display name, defaulting to the file's stem.
    pub fn install(
        &self,
        owner: i64,
        spool: &Path,
        filename: &str,
        name: Option<&str>,
        encoder: &Encoder,
    ) -> Result<Asset> {
        let ext = Path::new(filename)
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
            .unwrap_or_default();
        let Some(kind) = kind_of(&ext) else {
            return Err(Error::invalid(format!(
                "that file type is not accepted; send one of {}",
                allowed_exts().join(", ")
            )));
        };
        let bytes = std::fs::metadata(spool).map_err(Error::from)?.len();
        if bytes == 0 {
            return Err(Error::invalid("that file is empty"));
        }
        if bytes > MAX_MEDIA_BYTES as u64 {
            return Err(Error::TooLarge(format!(
                "the file is over the {} MB limit",
                MAX_MEDIA_BYTES >> 20
            )));
        }

        let probe = match encoder.probe(spool) {
            Ok(p) => p,
            Err(ytp_encode::EncodeError::Unavailable(detail)) => {
                return Err(Error::internal(format!("ffprobe is not available: {detail}")))
            }
            Err(ytp_encode::EncodeError::Failed(detail)) => {
                return Err(Error::invalid(format!(
                    "that file has no audio or video stream ffprobe can decode: {detail}"
                )))
            }
            Err(ytp_encode::EncodeError::Cancelled) => {
                return Err(Error::internal("ffprobe was cancelled"))
            }
        };
        if !probe.has_video && !probe.has_audio {
            return Err(Error::invalid(
                "that file has no audio or video stream ffprobe can decode",
            ));
        }

        let stem = clean_name(
            Path::new(filename)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .as_deref()
                .unwrap_or(""),
        );
        let name = name
            .map(clean_name)
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| if stem.is_empty() { "media".into() } else { stem });

        std::fs::create_dir_all(self.owner_dir(owner)).map_err(Error::from)?;
        let id = self.free_id(owner)?;
        let asset = Asset {
            id: id.clone(),
            name,
            ext: ext.clone(),
            kind,
            bytes,
            duration: (probe.duration.is_finite() && probe.duration > 0.0).then_some(probe.duration),
            width: probe.has_video.then_some(probe.width).filter(|w| *w > 0),
            height: probe.has_video.then_some(probe.height).filter(|h| *h > 0),
            video_codec: codec(&probe, "video"),
            audio_codec: codec(&probe, "audio"),
            uploaded_at: rfc3339(now()),
        };

        let path = self.file(owner, &id, &ext);
        std::fs::rename(spool, &path).map_err(Error::from)?;
        // The sidecar goes in beside it; a failure here removes the file again
        // rather than leave half an asset behind.
        let sidecar = self.sidecar(owner, &id);
        let tmp = self.owner_dir(owner).join(format!("{id}.{}.tmp", std::process::id()));
        let written = serde_json::to_vec(&asset)
            .map_err(Error::from)
            .and_then(|bytes| std::fs::write(&tmp, bytes).map_err(Error::from))
            .and_then(|_| std::fs::rename(&tmp, &sidecar).map_err(Error::from));
        if let Err(e) = written {
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        Ok(asset)
    }

    /// Delete `owner`'s asset; the asset that went.
    pub fn delete(&self, owner: i64, id: &str) -> Result<Asset> {
        let asset = self
            .get(owner, id)
            .ok_or_else(|| Error::not_found("no such media asset"))?;
        std::fs::remove_file(self.file(owner, id, &asset.ext)).map_err(Error::from)?;
        let _ = std::fs::remove_file(self.sidecar(owner, id));
        Ok(asset)
    }

    /// An id no file or sidecar of `owner` uses yet: 12 hex characters, like a
    /// MIDI file's id.
    fn free_id(&self, owner: i64) -> Result<String> {
        for _ in 0..16 {
            let id: String = rand::thread_rng()
                .gen::<[u8; 6]>()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            if !self.sidecar(owner, &id).exists()
                && allowed_exts()
                    .iter()
                    .all(|ext| !self.file(owner, &id, ext).exists())
            {
                return Ok(id);
            }
        }
        Err(Error::internal("cannot find a free media asset id"))
    }
}

/// A plain file: neither a directory nor a symlink.
fn is_regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_file())
        .unwrap_or(false)
}

fn codec(probe: &Probe, kind: &str) -> Option<String> {
    probe
        .streams
        .iter()
        .find(|s| s.kind == kind)
        .map(|s| s.codec.clone())
        .filter(|c| !c.is_empty())
}

fn modified(path: &Path) -> String {
    use std::time::UNIX_EPOCH;
    let secs = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or_else(now);
    rfc3339(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_streaming_cap_refuses_the_chunk_that_passes_it() {
        let mut meter = Meter::new(1024);
        meter.add(1024).unwrap();
        assert_eq!(meter.size(), 1024);
        // The very next byte is over: nothing past the cap is written.
        assert!(matches!(meter.add(1), Err(Error::TooLarge(_))));
        assert_eq!(meter.size(), 1025);
    }

    #[test]
    fn the_cap_is_200_mb() {
        assert_eq!(MAX_MEDIA_BYTES, 200 * 1024 * 1024);
    }

    #[test]
    fn only_the_accepted_extensions_have_a_kind() {
        for ext in allowed_exts() {
            assert!(kind_of(ext).is_some(), "{ext}");
        }
        for ext in ["txt", "json", "exe", "mp4.exe", ""] {
            assert!(kind_of(ext).is_none(), "{ext}");
        }
        assert_eq!(kind_of("m4a"), Some(Kind::Audio));
        assert_eq!(kind_of("jpeg"), Some(Kind::Image));
    }

    #[test]
    fn asset_ids_are_what_a_project_may_name() {
        assert!(valid_id("abc123"));
        assert!(valid_id("a-b_c"));
        assert!(!valid_id(""));
        assert!(!valid_id("../x"));
        assert!(!valid_id(&"a".repeat(65)));
    }
}
