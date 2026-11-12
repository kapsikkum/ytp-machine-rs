//! Everything the generator reads from a corpus database, loaded into memory
//! in one go (as `_pool` does in the Python app). Each table is read on its
//! own: a corpus made before one of them existed is an empty table, not an
//! error.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use sqlx::sqlite::SqliteConnection;
use sqlx::Row;
use ytp_core::Result;

use crate::{open_ro, CorpusStore, DB_NAME};

/// One `word_clips` / `noise_clips` row.
#[derive(Debug, Clone)]
pub struct ClipRow {
    pub id: i64,
    pub source_id: i64,
    pub word: String,
    pub start_time: f64,
    pub end_time: f64,
    /// As stored: relative to the corpus directory.
    pub source_file: String,
    pub edited: bool,
    /// JSON `[[phone, start, end], ...]`, seconds from `start_time`.
    pub phones: Option<String>,
}

/// One `clip_tone` row, features in `prosody::FEATURES` order.
#[derive(Debug, Clone)]
pub struct ToneRow {
    pub clip_id: i64,
    pub features: [Option<f64>; 7],
    pub category: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct CorpusData {
    pub name: String,
    pub dir: PathBuf,
    /// `ORDER BY source_id, start_time`.
    pub clips: Vec<ClipRow>,
    pub noises: Vec<ClipRow>,
    /// `(word, clip_id, score)`
    pub ratings: Vec<(String, i64, i64)>,
    /// `(clip_id, total reports)`
    pub boundary_reports: Vec<(i64, i64)>,
    pub titles: HashMap<i64, String>,
    pub settings: HashMap<String, String>,
    pub tone: Vec<ToneRow>,
    /// Raw JSON `[[char, start, end], ...]` per clip id.
    pub alignments: HashMap<i64, String>,
}

pub const TONE_FEATURES: [&str; 7] = [
    "pitch",
    "pitch_start",
    "pitch_end",
    "movement",
    "loudness",
    "pace",
    "brightness",
];

/// `p`, a stored path, made absolute against the corpus directory `base`.
/// `None` when it leads outside it (a bundle is untrusted input).
pub fn resolve_in(base: &Path, p: &str) -> Option<PathBuf> {
    let rel = p.replace('\\', "/");
    let mut out = base.to_path_buf();
    let depth0 = out.components().count();
    for c in Path::new(&rel).components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            Component::ParentDir => {
                if out.components().count() <= depth0 {
                    return None;
                }
                out.pop();
            }
            // Absolute paths and drive prefixes are outside the corpus.
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    // A symlink may still lead out: check where it really goes.
    if let (Ok(real), Ok(root)) = (out.canonicalize(), base.canonicalize()) {
        if !real.starts_with(&root) {
            return None;
        }
    }
    Some(out)
}

async fn clip_rows(
    conn: &mut SqliteConnection,
    table: &str,
    has_edited: bool,
    has_phones: bool,
    ordered: bool,
) -> Vec<ClipRow> {
    let sql = format!(
        "SELECT id, source_id, word, start_time, end_time, source_file, {}, {} FROM {table}{}",
        if has_edited { "COALESCE(edited, 0)" } else { "0" },
        if has_phones { "phones" } else { "NULL" },
        if ordered { " ORDER BY source_id, start_time" } else { "" },
    );
    let Ok(rows) = sqlx::query(&sql).fetch_all(&mut *conn).await else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|r| {
            Some(ClipRow {
                id: r.try_get(0).ok()?,
                source_id: r.try_get(1).ok()?,
                word: r.try_get(2).ok()?,
                start_time: r.try_get(3).ok()?,
                end_time: r.try_get(4).ok()?,
                source_file: r.try_get(5).ok()?,
                edited: r.try_get::<i64, _>(6).unwrap_or(0) != 0,
                phones: r.try_get::<Option<String>, _>(7).ok().flatten(),
            })
        })
        .collect()
}

async fn columns(conn: &mut SqliteConnection, table: &str) -> Vec<String> {
    sqlx::query_scalar("SELECT name FROM pragma_table_info(?)")
        .bind(table)
        .fetch_all(&mut *conn)
        .await
        .unwrap_or_default()
}

impl CorpusStore {
    /// Read corpus `name` into memory.
    pub async fn load(&self, name: &str) -> Result<CorpusData> {
        crate::validate_name(name).map_err(|_| ytp_core::Error::not_found("no such corpus"))?;
        let dir = self.dir(name);
        let db = dir.join(DB_NAME);
        if !db.is_file() {
            return Err(ytp_core::Error::not_found("no such corpus"));
        }
        let mut conn = open_ro(&db).await?;
        let data = read_all(&mut conn, name, dir).await;
        use sqlx::Connection;
        let _ = conn.close().await;
        Ok(data)
    }
}

async fn read_all(conn: &mut SqliteConnection, name: &str, dir: PathBuf) -> CorpusData {
    let wc = columns(conn, "word_clips").await;
    let clips = clip_rows(
        conn,
        "word_clips",
        wc.iter().any(|c| c == "edited"),
        wc.iter().any(|c| c == "phones"),
        true,
    )
    .await;
    let noises = clip_rows(conn, "noise_clips", false, false, false).await;

    let ratings = sqlx::query("SELECT word, clip_id, score FROM splice_ratings")
        .fetch_all(&mut *conn)
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|r| Some((r.try_get(0).ok()?, r.try_get(1).ok()?, r.try_get(2).ok()?)))
        .collect();
    let boundary_reports = sqlx::query(
        "SELECT clip_id, SUM(count) AS n FROM boundary_reports GROUP BY clip_id",
    )
    .fetch_all(&mut *conn)
    .await
    .unwrap_or_default()
    .iter()
    .filter_map(|r| Some((r.try_get(0).ok()?, r.try_get(1).ok()?)))
    .collect();
    let titles = sqlx::query("SELECT id, title FROM sources")
        .fetch_all(&mut *conn)
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|r| Some((r.try_get::<i64, _>(0).ok()?, r.try_get::<String, _>(1).ok()?)))
        .collect();
    let settings = sqlx::query("SELECT key, value FROM settings")
        .fetch_all(&mut *conn)
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|r| Some((r.try_get::<String, _>(0).ok()?, r.try_get::<String, _>(1).ok()?)))
        .collect();
    let tone = sqlx::query(&format!(
        "SELECT clip_id, {}, category FROM clip_tone",
        TONE_FEATURES.join(", ")
    ))
    .fetch_all(&mut *conn)
    .await
    .unwrap_or_default()
    .iter()
    .filter_map(|r| {
        let mut features = [None; 7];
        for (i, f) in features.iter_mut().enumerate() {
            *f = r.try_get::<Option<f64>, _>(i + 1).ok().flatten();
        }
        Some(ToneRow {
            clip_id: r.try_get(0).ok()?,
            features,
            category: r.try_get::<Option<i64>, _>(8).ok().flatten(),
        })
    })
    .collect();
    let alignments = sqlx::query("SELECT clip_id, alignment FROM clip_alignments")
        .fetch_all(&mut *conn)
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|r| Some((r.try_get::<i64, _>(0).ok()?, r.try_get::<String, _>(1).ok()?)))
        .collect();

    CorpusData {
        name: name.to_string(),
        dir,
        clips,
        noises,
        ratings,
        boundary_reports,
        titles,
        settings,
        tone,
        alignments,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_inside_only() {
        let base = std::env::temp_dir();
        assert_eq!(
            resolve_in(&base, "downloads\\a.mp4"),
            Some(base.join("downloads").join("a.mp4"))
        );
        assert_eq!(resolve_in(&base, "a/../b.mp4"), Some(base.join("b.mp4")));
        assert_eq!(resolve_in(&base, "../x.mp4"), None);
        assert_eq!(resolve_in(&base, "a/../../x.mp4"), None);
        assert_eq!(resolve_in(&base, "/etc/passwd"), None);
        assert_eq!(resolve_in(&base, "C:/Windows/x.mp4"), None);
    }
}
