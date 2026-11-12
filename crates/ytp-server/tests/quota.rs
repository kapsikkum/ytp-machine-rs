//! Removed options (sing, vocode), the per-role request quota, and the
//! per-corpus pronunciations and saved splice recipes read from an installed
//! corpus.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::http::{header, StatusCode};
use common::*;
use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
use sqlx::{ConnectOptions, Connection};
use ytp_media::{RenderError, RenderJob, Renderer};
use ytp_splice::{AudioProbe, NoAudio};

struct Quick;

impl Renderer for Quick {
    fn render(
        &self,
        job: &RenderJob,
        _: &dyn Fn(&str, usize, usize) -> bool,
    ) -> Result<(), RenderError> {
        std::fs::write(job.out, b"mp4").map_err(|e| RenderError::Failed(e.to_string()))
    }

    fn glitches(&self) -> Vec<String> {
        Vec::new()
    }

    fn audio(&self) -> &dyn AudioProbe {
        &NoAudio
    }

    fn check(&self) -> Result<String, String> {
        Ok("quick".into())
    }
}

async fn setup() -> (TestApp, Auth) {
    let t = TestApp::with_renderer(Arc::new(Quick), |c| c.job_workers = 2).await;
    let admin = t.setup_admin().await;
    let bundle = make_bundle(t.dir.path(), &[]).await;
    let r = t.install(&admin, &[("file", Some("fixture.tar.zst"), &bundle)]).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    (t, admin)
}

async fn wait_done(t: &TestApp, auth: &Auth, id: &str) -> Value {
    for _ in 0..200 {
        let r = t.get(&format!("/api/jobs/{id}"), auth).await;
        if matches!(r.body["state"].as_str(), Some("done" | "failed")) {
            return r.body;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("job {id} never finished");
}

#[tokio::test]
async fn sing_and_vocode_are_refused_not_silently_dropped() {
    let (t, admin) = setup().await;
    for options in [
        json!({ "sing": true }),
        json!({ "sing": false }),
        json!({ "vocode": true }),
        json!({ "sing_key": 3, "speed": 1.1 }),
        json!({ "sing_midi": "song.mid" }),
    ] {
        let r = t
            .post(
                "/api/ytp/generate",
                &admin,
                json!({ "text": "hello world", "voice": "fixture", "options": options }),
            )
            .await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{options}: {:?}", r.body);
        assert_eq!(r.code(), "invalid");
        let msg = r.body["message"].as_str().unwrap();
        assert!(msg.contains("removed") && msg.contains("reworked"), "{msg}");
    }
    // the inline effect is refused too
    let r = t
        .post("/api/ytp/generate", &admin, json!({ "text": "hello{vocode} world", "voice": "fixture" }))
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{:?}", r.body);
    assert!(r.body["message"].as_str().unwrap().contains("vocode"));

    // everything else still goes through, and the job shows no trace of them
    let r = t
        .post(
            "/api/ytp/generate",
            &admin,
            json!({ "text": "hello world", "voice": "fixture", "options": { "speed": 1.1, "seed": "s" } }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let job = wait_done(&t, &admin, r.body["job_id"].as_str().unwrap()).await;
    assert_eq!(job["state"], "done", "{job}");
    let keys: Vec<&String> = job["options"].as_object().unwrap().keys().collect();
    assert!(keys.iter().all(|k| !k.starts_with("sing") && *k != "vocode"), "{keys:?}");
    assert!(job["warnings"].as_array().unwrap().is_empty(), "{job}");
}

async fn role_with_rpm(t: &TestApp, admin: &Auth, name: &str, rpm: u32) -> Auth {
    let r = t
        .post(
            "/api/admin/roles",
            admin,
            json!({
                "name": name,
                "permissions": ["generate.ytp", "corpus.read"],
                "quota": { "max_concurrent_jobs": 8, "max_render_seconds": 60, "max_height": 480, "requests_per_minute": rpm }
            }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    t.create_user(admin, &format!("{name}-user"), &[name]).await
}

fn say() -> Value {
    json!({ "text": "hello world", "voice": "fixture", "captions": false })
}

#[tokio::test]
async fn requests_per_minute_limits_generation_with_a_retry_after() {
    let (t, admin) = setup().await;
    let slow = role_with_rpm(&t, &admin, "slow", 3).await;
    for n in 0..3 {
        let r = t.post("/api/ytp/generate", &slow, say()).await;
        assert_eq!(r.status, StatusCode::OK, "request {n}: {:?}", r.body);
    }
    let r = t.post("/api/ytp/generate", &slow, say()).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{:?}", r.body);
    assert_eq!(r.code(), "rate_limited");
    // 3 a minute: a token is back in 20 s
    let wait: u64 = r.headers[header::RETRY_AFTER].to_str().unwrap().parse().unwrap();
    assert!((1..=20).contains(&wait), "Retry-After {wait}");

    // reading is not throttled, and neither are other users
    assert_eq!(t.get("/api/corpora", &slow).await.status, StatusCode::OK);
    assert_eq!(t.get("/api/corpora", &slow).await.status, StatusCode::OK);
    assert_eq!(t.get("/api/corpora", &slow).await.status, StatusCode::OK);
    assert_eq!(t.get("/api/corpora", &slow).await.status, StatusCode::OK);
    let r = t.post("/api/ytp/generate", &admin, say()).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
}

#[tokio::test]
async fn a_zero_quota_turns_generating_off_and_the_best_role_wins() {
    let (t, admin) = setup().await;
    let off = role_with_rpm(&t, &admin, "off", 0).await;
    let r = t.post("/api/ytp/generate", &off, say()).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{:?}", r.body);
    assert_eq!(r.code(), "forbidden");

    // a user holding both the 0 and a 2-a-minute role gets 2
    role_with_rpm(&t, &admin, "two", 2).await;
    let both = t.create_user(&admin, "both", &["off", "two"]).await;
    assert_eq!(t.post("/api/ytp/generate", &both, say()).await.status, StatusCode::OK);
    assert_eq!(t.post("/api/ytp/generate", &both, say()).await.status, StatusCode::OK);
    assert_eq!(
        t.post("/api/ytp/generate", &both, say()).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
}

async fn corpus_with_pronunciations_and_recipes(dir: &std::path::Path) -> Vec<u8> {
    let db = dir.join("with-extras.db");
    let _ = std::fs::remove_file(&db);
    make_corpus_db(&db).await;
    let mut conn = SqliteConnectOptions::new()
        .filename(&db)
        .journal_mode(SqliteJournalMode::Delete)
        .connect()
        .await
        .unwrap();
    for sql in [
        "CREATE TABLE splice_recipes (word TEXT PRIMARY KEY, recipe TEXT NOT NULL)",
        r#"INSERT INTO splice_recipes VALUES ('cap', '[{"phones":["K","AE"],"from":["cat"]},{"phones":["P"],"from":["top"],"clip_id":2,"start":1.0,"end":1.1}]')"#,
        "INSERT INTO splice_recipes VALUES ('broken', 'not json')",
    ] {
        sqlx::query(sql).execute(&mut conn).await.unwrap();
    }
    conn.close().await.unwrap();
    let mut b = tar::Builder::new(Vec::new());
    raw_entry(&mut b, "corpus.db", tar::EntryType::Regular, &std::fs::read(&db).unwrap(), None);
    raw_entry(&mut b, "pronunciations.csv", tar::EntryType::Regular, b"tomato,T AH M AA T OW\n", None);
    let tar = b.into_inner().unwrap();
    zstd::encode_all(&tar[..], 3).unwrap()
}

#[tokio::test]
async fn an_installed_corpus_brings_its_pronunciations_and_recipes() {
    let t = TestApp::with_renderer(Arc::new(Quick), |_| {}).await;
    let admin = t.setup_admin().await;
    let bundle = corpus_with_pronunciations_and_recipes(t.dir.path()).await;
    let r = t.install(&admin, &[("file", Some("rosen.tar.zst"), &bundle)]).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);

    // the shared file sits in the data directory, the corpus's own wins
    std::fs::write(
        t.st.config.data_dir.join("pronunciations.csv"),
        "tomato,T AH M EY T OW\nwii,wee\n",
    )
    .unwrap();

    let data = t.st.corpora.load("rosen").await.unwrap();
    assert_eq!(data.recipes.len(), 2);
    assert_eq!(data.pronunciations.as_deref(), Some("tomato,T AH M AA T OW\n"));
    assert!(data.shared_pronunciations.as_deref().unwrap().contains("wii"));

    let pool = ytp_splice::Pool::build(&data);
    assert_eq!(pool.recipes.len(), 1, "the unreadable recipe is skipped");
    assert_eq!(pool.recipes["cap"][1].cut.unwrap().clip_id, 2);
    let _scope = pool.scope();
    let say = |w: &str| ytp_text::word_to_phonemes(w).unwrap().join(" ");
    assert_eq!(say("tomato"), "T AH M AA T OW");
    assert_eq!(say("wii"), "W IY");

    // editing a file changes the version a cached pool is checked against
    let before = t.st.corpora.version("rosen").unwrap();
    std::thread::sleep(Duration::from_millis(30));
    std::fs::write(t.st.corpora.dir("rosen").join("pronunciations.csv"), "tomato,T AH M AH T OW\n").unwrap();
    assert_ne!(t.st.corpora.version("rosen").unwrap(), before);
}

#[tokio::test]
async fn a_job_speaks_with_the_corpus_pronunciations() {
    // "howld" is in no dictionary: the corpus file says it sounds like "hello"
    // + "world", so it is built from them instead of being missing.
    let t = TestApp::with_renderer(Arc::new(Quick), |_| {}).await;
    let admin = t.setup_admin().await;
    let bundle = common::make_bundle(
        t.dir.path(),
        &[("pronunciations.csv", tar::EntryType::Regular, b"howld,HH AH L OW W ER L D\n", None)],
    )
    .await;
    let r = t.install(&admin, &[("file", Some("fixture.tar.zst"), &bundle)]).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let r = t
        .post(
            "/api/ytp/generate",
            &admin,
            json!({ "text": "howld", "voice": "fixture", "captions": false, "options": { "seed": "x" } }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let job = wait_done(&t, &admin, r.body["job_id"].as_str().unwrap()).await;
    // spliced from the pieces "hello" and "world" rather than missing
    assert_eq!(job["state"], "done", "{job}");
    assert_eq!(job["report"]["tokens"][0]["status"], "spliced", "{job}");

    // the same corpus without the file cannot say it
    let plain = common::make_bundle(t.dir.path(), &[]).await;
    let r = t.install(&admin, &[("file", Some("plain.tar.zst"), &plain)]).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let r = t
        .post("/api/ytp/generate", &admin, json!({ "text": "howld", "voice": "plain", "captions": false }))
        .await;
    let job = wait_done(&t, &admin, r.body["job_id"].as_str().unwrap()).await;
    assert_eq!(job["state"], "failed", "{job}");
    assert_eq!(job["report"]["missing"], json!(["howld"]));
}

#[tokio::test]
async fn the_clip_cache_lives_under_the_data_dir_and_can_be_turned_off() {
    let on = TestApp::new().await;
    assert!(on.st.config.data_dir.join("cache").join("clips").is_dir());
    let off = TestApp::with_config(|c| c.clip_cache_mb = 0).await;
    assert!(!off.st.config.data_dir.join("cache").join("clips").exists());
}

#[tokio::test]
async fn a_refused_request_does_not_spend_the_quota() {
    let (t, admin) = setup().await;
    let r = t
        .post(
            "/api/admin/roles",
            &admin,
            json!({ "name": "reader", "permissions": ["corpus.read"],
                    "quota": { "max_concurrent_jobs": 1, "max_render_seconds": 60, "max_height": 480, "requests_per_minute": 1 } }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let reader = t.create_user(&admin, "reader-user", &["reader"]).await;
    for _ in 0..3 {
        let r = t.post("/api/ytp/generate", &reader, say()).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{:?}", r.body);
    }
}
