//! Installed speech corpora: `<data-dir>/corpora/<name>/corpus.db` plus
//! media, as produced by the Python app. Read-only apart from install/delete.

pub mod bundle;
pub mod data;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use rand::Rng;
use serde::Serialize;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use sqlx::{ConnectOptions, Connection};
use ytp_core::time::{now, rfc3339};
use ytp_core::{Error, Result};

pub use bundle::{Limits, DB_NAME};
pub use data::{ClipRow, CorpusData, ToneRow};

const MARKER: &str = ".ytp-installed";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Corpus {
    pub name: String,
    pub words: i64,
    pub clips: i64,
    pub sources: i64,
    pub size_bytes: u64,
    pub installed_at: String,
}

/// Corpus names are directory names: keep them boring.
pub fn validate_name(name: &str) -> Result<()> {
    let ok = (1..=64).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(
            "corpus name must be 1-64 characters of lowercase letters, digits, '_' or '-'",
        ))
    }
}

/// A corpus name from an uploaded file name: `Michael Rosen.tar.zst` -> `michael-rosen`.
pub fn slug_from_filename(filename: &str) -> String {
    let base = filename.rsplit(['/', '\\']).next().unwrap_or("");
    let lower = base.to_lowercase();
    let stem = [".tar.zst", ".tzst", ".tar", ".zst"]
        .iter()
        .find_map(|ext| lower.strip_suffix(ext))
        .unwrap_or(&lower);
    let mut out = String::new();
    for c in stem.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out
        .trim_matches(|c| c == '-' || c == '_')
        .chars()
        .take(64)
        .collect();
    if out.is_empty() {
        "corpus".into()
    } else {
        out
    }
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&p) else {
            continue;
        };
        for e in rd.flatten() {
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(e.path()),
                Ok(t) if t.is_file() => total += e.metadata().map(|m| m.len()).unwrap_or(0),
                _ => {}
            }
        }
    }
    total
}

fn random_suffix() -> String {
    let n: u64 = rand::thread_rng().gen();
    format!("{n:016x}")
}

async fn open_ro(db: &Path) -> Result<SqliteConnection> {
    SqliteConnectOptions::new()
        .filename(db)
        .read_only(true)
        .connect()
        .await
        .map_err(|e| Error::invalid(format!("not a readable corpus database: {e}")))
}

async fn count(conn: &mut SqliteConnection, sql: &str) -> Result<i64> {
    Ok(sqlx::query_scalar(sql).fetch_one(&mut *conn).await?)
}

/// The database must be intact and have what every consumer reads.
async fn validate_db(db: &Path) -> Result<()> {
    if !db.is_file() {
        return Err(Error::invalid("bundle must contain a corpus database"));
    }
    let mut conn = open_ro(db).await?;
    let res = validate_conn(&mut conn).await;
    let _ = conn.close().await;
    res
}

async fn validate_conn(conn: &mut SqliteConnection) -> Result<()> {
    let bad = |_| Error::invalid("bundle database is not readable");
    let check: String = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_one(&mut *conn)
        .await
        .map_err(bad)?;
    if check != "ok" {
        return Err(Error::invalid("bundle database failed its integrity check"));
    }
    for (table, required) in [
        ("sources", &["id", "video_id", "source_file"][..]),
        (
            "word_clips",
            &[
                "id",
                "source_id",
                "word",
                "start_time",
                "end_time",
                "source_file",
            ][..],
        ),
    ] {
        let cols: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info(?)")
            .bind(table)
            .fetch_all(&mut *conn)
            .await
            .map_err(bad)?;
        if cols.is_empty() {
            return Err(Error::invalid(format!(
                "bundle database is missing the {table} table"
            )));
        }
        if let Some(c) = required.iter().find(|c| !cols.iter().any(|x| x == *c)) {
            return Err(Error::invalid(format!(
                "bundle database table {table} lacks column {c}"
            )));
        }
    }
    Ok(())
}

struct Cached {
    db_mtime: SystemTime,
    corpus: Corpus,
}

pub struct CorpusStore {
    root: PathBuf,
    limits: Limits,
    cache: Mutex<HashMap<String, Cached>>,
    /// Serialises install/delete so two of them never race on one name.
    write_lock: tokio::sync::Mutex<()>,
}

impl CorpusStore {
    /// `root` is the `corpora` directory. Leftovers of interrupted installs
    /// and deletes are removed.
    pub fn open(root: impl Into<PathBuf>, limits: Limits) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        for e in std::fs::read_dir(&root)?.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with(".staging-") || n.starts_with(".trash-") || n.starts_with(".upload-") {
                let p = e.path();
                let _ = if p.is_dir() {
                    std::fs::remove_dir_all(&p)
                } else {
                    std::fs::remove_file(&p)
                };
            }
        }
        Ok(Self {
            root,
            limits,
            cache: Mutex::new(HashMap::new()),
            write_lock: tokio::sync::Mutex::new(()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn dir(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// Path for a temporary upload, on the same volume as the corpora so
    /// the final rename is atomic.
    pub fn upload_path(&self) -> PathBuf {
        self.root.join(format!(".upload-{}", random_suffix()))
    }

    pub async fn list(&self) -> Result<Vec<Corpus>> {
        let mut names = Vec::new();
        for e in std::fs::read_dir(&self.root)?.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if e.path().is_dir() && validate_name(&n).is_ok() {
                names.push(n);
            }
        }
        names.sort();
        let mut out = Vec::new();
        for n in names {
            match self.get(&n).await {
                Ok(c) => out.push(c),
                Err(e) => tracing::warn!(corpus = %n, error = %e, "skipping unreadable corpus"),
            }
        }
        Ok(out)
    }

    pub async fn get(&self, name: &str) -> Result<Corpus> {
        validate_name(name).map_err(|_| Error::not_found("no such corpus"))?;
        let dir = self.dir(name);
        let db = dir.join(DB_NAME);
        let mtime = std::fs::metadata(&db)
            .and_then(|m| m.modified())
            .map_err(|_| Error::not_found("no such corpus"))?;
        if let Some(c) = self.cache.lock().unwrap().get(name) {
            if c.db_mtime == mtime {
                return Ok(c.corpus.clone());
            }
        }
        let mut conn = open_ro(&db).await?;
        let counts = async {
            Ok::<_, Error>((
                count(&mut conn, "SELECT count(DISTINCT word) FROM word_clips").await?,
                count(&mut conn, "SELECT count(*) FROM word_clips").await?,
                count(&mut conn, "SELECT count(*) FROM sources").await?,
            ))
        }
        .await;
        let _ = conn.close().await;
        let (words, clips, sources) = counts?;
        let installed = std::fs::read_to_string(dir.join(MARKER))
            .ok()
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or_else(|| {
                std::fs::metadata(&dir)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs() as i64)
            });
        let size_bytes = tokio::task::spawn_blocking(move || dir_size(&dir))
            .await
            .map_err(Error::internal)?;
        let corpus = Corpus {
            name: name.to_string(),
            words,
            clips,
            sources,
            size_bytes,
            installed_at: rfc3339(installed),
        };
        self.cache.lock().unwrap().insert(
            name.to_string(),
            Cached {
                db_mtime: mtime,
                corpus: corpus.clone(),
            },
        );
        Ok(corpus)
    }

    /// Most frequent words in a corpus with their clip counts.
    pub async fn words(&self, name: &str, limit: i64) -> Result<Vec<(String, i64)>> {
        validate_name(name).map_err(|_| Error::not_found("no such corpus"))?;
        let db = self.dir(name).join(DB_NAME);
        if !db.is_file() {
            return Err(Error::not_found("no such corpus"));
        }
        let mut conn = open_ro(&db).await?;
        let rows = sqlx::query_as(
            "SELECT word, count(*) AS n FROM word_clips GROUP BY word ORDER BY n DESC, word LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&mut conn)
        .await;
        let _ = conn.close().await;
        Ok(rows?)
    }

    /// Install a bundle file as corpus `name`. The bundle is unpacked and
    /// checked in a staging directory, then renamed into place; nothing is
    /// visible under `name` until it is complete. With `replace`, an
    /// existing corpus of that name is swapped out.
    pub async fn install(&self, bundle_path: &Path, name: &str, replace: bool) -> Result<Corpus> {
        validate_name(name)?;
        let _guard = self.write_lock.lock().await;
        let target = self.dir(name);
        if target.exists() && !replace {
            return Err(Error::conflict("a corpus with that name already exists"));
        }
        let staging = self.root.join(format!(".staging-{}", random_suffix()));
        std::fs::create_dir(&staging)?;
        let res = self.stage(bundle_path, &staging).await;
        let res = match res {
            Ok(()) => self.swap_in(&staging, &target),
            Err(e) => Err(e),
        };
        if staging.exists() {
            let _ = std::fs::remove_dir_all(&staging);
        }
        res?;
        self.cache.lock().unwrap().remove(name);
        self.get(name).await
    }

    async fn stage(&self, bundle_path: &Path, staging: &Path) -> Result<()> {
        let (b, s, limits) = (
            bundle_path.to_path_buf(),
            staging.to_path_buf(),
            self.limits,
        );
        tokio::task::spawn_blocking(move || bundle::extract(&b, &s, limits))
            .await
            .map_err(Error::internal)??;
        validate_db(&staging.join(DB_NAME)).await?;
        let vjson = staging.join("voice").join("voice.json");
        if vjson.is_file() {
            let parsed: serde_json::Value = serde_json::from_slice(&std::fs::read(&vjson)?)
                .map_err(|_| Error::invalid("bundle voice.json is not valid JSON"))?;
            if !parsed.is_object() {
                return Err(Error::invalid("bundle voice.json must be a JSON object"));
            }
        }
        std::fs::write(staging.join(MARKER), now().to_string())?;
        Ok(())
    }

    fn swap_in(&self, staging: &Path, target: &Path) -> Result<()> {
        if !target.exists() {
            std::fs::rename(staging, target)?;
            return Ok(());
        }
        let trash = self.root.join(format!(".trash-{}", random_suffix()));
        std::fs::rename(target, &trash)?;
        if let Err(e) = std::fs::rename(staging, target) {
            let _ = std::fs::rename(&trash, target);
            return Err(e.into());
        }
        let _ = std::fs::remove_dir_all(&trash);
        Ok(())
    }

    pub async fn delete(&self, name: &str) -> Result<()> {
        validate_name(name).map_err(|_| Error::not_found("no such corpus"))?;
        let _guard = self.write_lock.lock().await;
        let target = self.dir(name);
        if !target.is_dir() {
            return Err(Error::not_found("no such corpus"));
        }
        // Rename first so the corpus disappears at once, then clean up.
        let trash = self.root.join(format!(".trash-{}", random_suffix()));
        std::fs::rename(&target, &trash)?;
        self.cache.lock().unwrap().remove(name);
        tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&trash))
            .await
            .map_err(Error::internal)??;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(validate_name("michael-rosen").is_ok());
        assert!(validate_name("..").is_err());
        assert!(validate_name(".staging-x").is_err());
        assert!(validate_name("A").is_err());
        assert_eq!(
            slug_from_filename("C:\\x\\Michael Rosen.tar.zst"),
            "michael-rosen"
        );
        assert_eq!(slug_from_filename("../../.tar.zst"), "corpus");
        assert_eq!(
            slug_from_filename("corpus-2026-01-01.tar.zst"),
            "corpus-2026-01-01"
        );
    }
}
