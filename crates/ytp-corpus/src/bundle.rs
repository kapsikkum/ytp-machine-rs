//! Safe extraction of a corpus bundle (`.tar.zst`), mirroring the Python
//! app's `scripts/corpus.py` rules: only the known corpus layout, only
//! regular files and directories, only portable relative paths.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::Path;

use tar::EntryType;
use ytp_core::{Error, Result};

pub const DB_NAME: &str = "corpus.db";
pub const LEGACY_DB_NAME: &str = "michael_rosen.db";
const DIR_MEMBERS: &[&str] = &["downloads", "transcripts", "voice"];
const FILE_MEMBERS: &[&str] = &[DB_NAME, LEGACY_DB_NAME, "pronunciations.csv"];

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_unpacked_bytes: u64,
    pub max_members: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_unpacked_bytes: 8 << 30,
            max_members: 100_000,
        }
    }
}

fn bad(msg: impl Into<String>) -> Error {
    Error::invalid(msg)
}

fn is_reserved(part: &str) -> bool {
    let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

/// Check one member path; returns its components.
pub fn check_path(raw: &str, is_dir: bool, size: u64) -> Result<Vec<String>> {
    let mut name = raw.trim_end_matches('/');
    while let Some(rest) = name.strip_prefix("./") {
        name = rest;
    }
    if name.is_empty() || name == "." {
        return Ok(Vec::new()); // the archive root itself
    }
    let parts: Vec<&str> = name.split('/').collect();
    let unsafe_path = name.contains('\\')
        || name.contains('\0')
        || parts.iter().any(|p| {
            p.is_empty()
                || *p == "."
                || *p == ".."
                || p.contains(':')
                || p.ends_with('.')
                || p.ends_with(' ')
                || p.chars().any(|c| c.is_control())
        });
    if unsafe_path {
        return Err(bad("bundle contains a nonportable or unsafe path"));
    }
    if parts.iter().any(|p| is_reserved(p)) {
        return Err(bad("bundle contains a reserved device filename"));
    }
    let top = parts[0];
    let is_dir_member = DIR_MEMBERS.contains(&top);
    if !is_dir_member && !FILE_MEMBERS.contains(&top) {
        return Err(bad(format!("unexpected bundle member `{top}`")));
    }
    if !is_dir_member && (parts.len() != 1 || is_dir) {
        return Err(bad("bundle files must have the expected corpus layout"));
    }
    if top == "voice" {
        let ok = if is_dir {
            parts == ["voice"] || parts == ["voice", "refs"]
        } else if parts == ["voice", "voice.json"] {
            size <= 1 << 20
        } else if parts == ["voice", "adapter.safetensors"] {
            size <= 256 << 20
        } else {
            parts.len() == 3
                && parts[1] == "refs"
                && parts[2].to_ascii_lowercase().ends_with(".wav")
                && size <= 16 << 20
        };
        if !ok {
            return Err(bad(format!("unexpected or oversized voice entry `{name}`")));
        }
    }
    Ok(parts.into_iter().map(String::from).collect())
}

/// Extract `bundle` into the (empty, existing) directory `dest`.
pub fn extract(bundle: &Path, dest: &Path, limits: Limits) -> Result<()> {
    let file = File::open(bundle)?;
    let decoder = zstd::Decoder::new(file).map_err(|e| bad(format!("not a zstd stream: {e}")))?;
    extract_from(decoder, dest, limits)
}

pub fn extract_from<R: Read>(reader: R, dest: &Path, limits: Limits) -> Result<()> {
    let unreadable = |e: io::Error| bad(format!("could not read bundle: {e}"));
    let mut archive = tar::Archive::new(reader);
    let mut seen = HashSet::new();
    let mut members = 0usize;
    let mut total = 0u64;

    for entry in archive.entries().map_err(unreadable)? {
        let mut entry = entry.map_err(unreadable)?;
        let kind = entry.header().entry_type();
        let is_dir = match kind {
            EntryType::Regular => false,
            EntryType::Directory => true,
            // PAX global headers carry metadata only.
            EntryType::XGlobalHeader => continue,
            _ => return Err(bad("bundle links and special files are not supported")),
        };
        members += 1;
        if members > limits.max_members {
            return Err(bad("bundle has too many members"));
        }
        let raw = String::from_utf8(entry.path_bytes().into_owned())
            .map_err(|_| bad("bundle contains a non-UTF-8 path"))?;
        let size = entry.header().size().map_err(unreadable)?;
        let parts = check_path(&raw, is_dir, size)?;
        if parts.is_empty() {
            continue;
        }
        if !seen.insert(parts.join("/").to_lowercase()) {
            return Err(bad("bundle contains a duplicate member path"));
        }
        let target = parts.iter().fold(dest.to_path_buf(), |p, c| p.join(c));
        if is_dir {
            fs::create_dir_all(&target)?;
            continue;
        }
        total = total.saturating_add(size);
        if total > limits.max_unpacked_bytes {
            return Err(bad("bundle exceeds the maximum unpacked size"));
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        // create_new: never follow or overwrite anything already there.
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)?;
        let copied = io::copy(&mut (&mut entry).take(size), &mut out).map_err(unreadable)?;
        if copied != size {
            return Err(bad("bundle is truncated"));
        }
    }
    // Old bundles name the database after one speaker; this app reads one name.
    let legacy = dest.join(LEGACY_DB_NAME);
    if legacy.exists() {
        if dest.join(DB_NAME).exists() {
            fs::remove_file(&legacy)?;
        } else {
            fs::rename(&legacy, dest.join(DB_NAME))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_rules() {
        assert_eq!(
            check_path("corpus.db", false, 1).unwrap(),
            vec!["corpus.db"]
        );
        assert!(check_path("./corpus.db", false, 1).is_ok());
        assert!(check_path("downloads/a/b.mp4", false, 1).is_ok());
        assert!(check_path("downloads/", true, 0).is_ok());
        for evil in [
            "../x",
            "/etc/passwd",
            "downloads/../../x",
            "downloads\\x",
            "C:/x",
            "downloads/a:b",
            "downloads/x.",
            "downloads/NUL.txt",
            "downloads//x",
            "evil.txt",
            "corpus.db/x",
        ] {
            assert!(check_path(evil, false, 1).is_err(), "{evil}");
        }
        assert!(check_path("corpus.db", true, 0).is_err());
        assert!(check_path("voice/refs/a.wav", false, 10).is_ok());
        assert!(check_path("voice/refs/a.mp3", false, 10).is_err());
        assert!(check_path("voice/other", true, 0).is_err());
        assert!(check_path("voice/voice.json", false, 2 << 20).is_err());
    }
}
