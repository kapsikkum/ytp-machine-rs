//! Timing of a repeated sentence with and without the clip cache, through the
//! real pipeline and a real ffmpeg. Slow (it makes a long synthetic video), so
//! it only runs on request:
//!
//!   cargo test -p ytp-server --test clip_cache_bench --release -- --ignored --nocapture
//!
//! Environment: `BENCH_MINUTES` (source length, default 15), `BENCH_SIZE`
//! (default 1280x720), `BENCH_GOP` (keyframe interval in frames, default 100),
//! `BENCH_SOURCE` (reuse a source file made earlier), `BENCH_RUNS` (default 4).

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::*;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
use sqlx::{ConnectOptions, Connection};
use ytp_core::{Event, JobState};

const SENTENCE: &str = "the quick brown fox jumps over the lazy dog and runs away from the big red barn";

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn make_source() -> PathBuf {
    if let Ok(p) = std::env::var("BENCH_SOURCE") {
        return PathBuf::from(p);
    }
    let dir = std::env::temp_dir().join("ytp-clip-cache-bench");
    std::fs::create_dir_all(&dir).unwrap();
    let (minutes, size, gop) = (env_or("BENCH_MINUTES", "15"), env_or("BENCH_SIZE", "1280x720"), env_or("BENCH_GOP", "100"));
    let path = dir.join(format!("long-{minutes}m-{size}-g{gop}.mp4"));
    if path.is_file() {
        return path;
    }
    let secs = minutes.parse::<u64>().unwrap() * 60;
    eprintln!("making a {minutes} min {size} source (once)...");
    let t = Instant::now();
    let ok = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=size={size}:rate=25"))
        .args(["-f", "lavfi", "-i", "sine=frequency=330:sample_rate=44100", "-t", &secs.to_string()])
        .args(["-pix_fmt", "yuv420p", "-c:v", "libx264", "-preset", "ultrafast", "-crf", "28", "-g", &gop])
        .args(["-c:a", "aac"])
        .arg(&path)
        .status()
        .unwrap()
        .success();
    assert!(ok, "could not make the source");
    eprintln!("  made in {:.1} s", t.elapsed().as_secs_f64());
    path
}

/// An installed corpus whose words sit a minute apart in one long video.
async fn install_corpus(root: &Path, source: &Path, minutes: u64) {
    let dir = root.join("bench");
    std::fs::create_dir_all(dir.join("downloads")).unwrap();
    let video = dir.join("downloads").join("long.mp4");
    if std::fs::hard_link(source, &video).is_err() {
        std::fs::copy(source, &video).unwrap();
    }
    let db = dir.join("corpus.db");
    let mut conn = SqliteConnectOptions::new()
        .filename(&db)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Delete)
        .connect()
        .await
        .unwrap();
    for sql in [
        "CREATE TABLE sources (id INTEGER PRIMARY KEY AUTOINCREMENT, video_id TEXT NOT NULL,
            source_file TEXT NOT NULL, title TEXT, url TEXT, ingested_at TEXT DEFAULT (datetime('now')))",
        "CREATE TABLE word_clips (id INTEGER PRIMARY KEY AUTOINCREMENT, source_id INTEGER NOT NULL,
            word TEXT NOT NULL, start_time REAL NOT NULL, end_time REAL NOT NULL, source_file TEXT NOT NULL)",
        "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        "INSERT INTO settings VALUES ('schema_version', '2')",
        "INSERT INTO sources (video_id, source_file) VALUES ('v1', 'downloads/long.mp4')",
    ] {
        sqlx::query(sql).execute(&mut conn).await.unwrap();
    }
    let mut words: Vec<&str> = SENTENCE.split(' ').collect();
    words.sort();
    words.dedup();
    let span = (minutes * 60 - 20) as f64;
    for (i, w) in words.iter().enumerate() {
        // spread through the file, never next to each other
        let at = 10.0 + span * (i as f64 + 0.5) / words.len() as f64;
        sqlx::query("INSERT INTO word_clips (source_id, word, start_time, end_time, source_file) VALUES (1, ?, ?, ?, 'downloads/long.mp4')")
            .bind(w)
            .bind(at)
            .bind(at + 0.45)
            .execute(&mut conn)
            .await
            .unwrap();
    }
    conn.close().await.unwrap();
}

struct Timing {
    total: f64,
    resolve: f64,
    encode: f64,
}

async fn render_once(t: &TestApp, admin: &Auth) -> Timing {
    let mut events = t.st.events.subscribe();
    let began = Instant::now();
    let r = t
        .post(
            "/api/ytp/generate",
            admin,
            json!({ "text": SENTENCE, "voice": "bench", "captions": false, "options": { "seed": "bench", "phrases": false } }),
        )
        .await;
    assert_eq!(r.status, 200, "{:?}", r.body);
    let id = r.body["job_id"].as_str().unwrap().to_string();
    let mut encoding_at = None;
    loop {
        match tokio::time::timeout(Duration::from_secs(300), events.recv()).await {
            Ok(Ok(Event::Job { id: eid, state, message, .. })) if eid == id => {
                if encoding_at.is_none() && message.as_deref().is_some_and(|m| m.starts_with("encoding")) {
                    encoding_at = Some(began.elapsed().as_secs_f64());
                }
                match state {
                    JobState::Done => break,
                    JobState::Failed | JobState::Cancelled => panic!("job ended {state:?}: {message:?}"),
                    _ => {}
                }
            }
            Ok(Ok(_)) | Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            other => panic!("no job event: {other:?}"),
        }
    }
    let total = began.elapsed().as_secs_f64();
    let resolve = encoding_at.unwrap_or(total);
    Timing { total, resolve, encode: total - resolve }
}

async fn run(cache_mb: u64, source: &Path, minutes: u64, runs: usize) -> Vec<Timing> {
    let t = TestApp::with_config(|c| c.clip_cache_mb = cache_mb).await;
    install_corpus(t.st.corpora.root(), source, minutes).await;
    let admin = t.setup_admin().await;
    let mut out = Vec::new();
    for _ in 0..runs {
        out.push(render_once(&t, &admin).await);
        // let the cache finish filling before the sentence comes round again
        let r = t.st.renderer.clone();
        tokio::task::spawn_blocking(move || r.wait_idle(Duration::from_secs(600))).await.unwrap();
    }
    if cache_mb > 0 {
        let dir = t.st.config.data_dir.join("cache").join("clips");
        let files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        let bytes: u64 = files.iter().map(|f| f.metadata().unwrap().len()).sum();
        eprintln!("  cache: {} files, {:.1} MB", files.len(), bytes as f64 / 1e6);
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "benchmark: needs ffmpeg and makes a long video"]
async fn repeated_sentence_with_and_without_the_clip_cache() {
    let minutes: u64 = env_or("BENCH_MINUTES", "15").parse().unwrap();
    let runs: usize = env_or("BENCH_RUNS", "4").parse().unwrap();
    let source = make_source();
    eprintln!("source {} ({:.0} MB)", source.display(), std::fs::metadata(&source).unwrap().len() as f64 / 1e6);
    eprintln!("sentence: {SENTENCE:?} ({} words)", SENTENCE.split(' ').count());
    for (label, mb) in [("cache off", 0u64), ("cache on ", 500)] {
        eprintln!("{label}");
        let times = run(mb, &source, minutes, runs).await;
        for (i, x) in times.iter().enumerate() {
            eprintln!(
                "  render {}: {:6.2} s total = {:5.2} choosing clips + {:5.2} encoding",
                i + 1,
                x.total,
                x.resolve,
                x.encode
            );
        }
        let warm: Vec<&Timing> = times.iter().skip(1).collect();
        let n = warm.len().max(1) as f64;
        eprintln!(
            "  repeats average: {:.2} s total = {:.2} choosing + {:.2} encoding",
            warm.iter().map(|x| x.total).sum::<f64>() / n,
            warm.iter().map(|x| x.resolve).sum::<f64>() / n,
            warm.iter().map(|x| x.encode).sum::<f64>() / n,
        );
    }
}
