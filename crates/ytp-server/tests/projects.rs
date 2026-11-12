//! Timeline projects over HTTP: ownership and sharing (no media needed), then
//! the whole chain with a real ffmpeg and a synthetic corpus — plan → project
//! → preview → render, for a sentence and for a song.

mod common;

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use common::*;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
use sqlx::{ConnectOptions, Connection};
use tower::ServiceExt;

// ── Plain projects: who may do what ──────────────────────────────────────────

fn doc(name: &str) -> Value {
    json!({
        "name": name,
        "corpus": "music",
        "tracks": [{
            "id": "video", "kind": "video", "name": "Video",
            "clips": [{
                "id": "c0", "start": 0.0, "duration": 0.5,
                "source": { "type": "corpus_clip", "corpus": "music", "clip_id": 1, "file": "downloads/v1.mp4", "in": 0.0, "out": 0.5 },
                "effects": [{ "type": "reverse" }, { "type": "pitch", "semitones": 2.0 }],
            }]
        }]
    })
}

async fn patch(t: &TestApp, who: &Auth, url: &str, body: Value) -> Resp {
    t.call(Method::PATCH, url, who, Some(body)).await
}

async fn user(t: &TestApp, admin: &Auth, name: &str, role: &str) -> Auth {
    t.create_user(admin, name, &[role]).await
}

#[tokio::test]
async fn projects_belong_to_their_owner_and_are_shared_by_permission() {
    let t = TestApp::new().await;
    let admin = t.setup_admin().await;
    let bob = user(&t, &admin, "bob", "editor").await; // may share
    let carol = user(&t, &admin, "carol", "creator").await; // may not
    let guest = user(&t, &admin, "gus", "guest").await; // reads only

    let r = t.post("/api/projects", &bob, json!({ "project": doc("First") })).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let id = r.body["id"].as_i64().unwrap();
    assert_eq!(r.body["access"], "owner");
    assert_eq!(r.body["kind"], "sequence");
    assert_eq!(r.body["revision"], 1);
    assert_eq!(r.body["project"]["id"], id);
    assert_eq!(r.body["project"]["tracks"][0]["clips"][0]["effects"][1]["semitones"], 2.0);
    let url = format!("/api/projects/{id}");

    // Only the owner sees it.
    assert_eq!(t.get(&url, &carol).await.status, StatusCode::NOT_FOUND);
    assert_eq!(t.get(&url, &admin).await.status, StatusCode::NOT_FOUND, "even an admin has to be given a project");
    assert_eq!(t.get("/api/projects", &carol).await.body, json!([]));
    let list = t.get("/api/projects", &bob).await;
    assert_eq!(list.body[0]["name"], "First");
    assert_eq!(list.body[0]["owner_name"], "bob");

    // A bad document is refused in words, and nothing is stored.
    let mut bad = doc("Bad");
    bad["tracks"][0]["clips"][0]["duration"] = json!(-1.0);
    let r = t.post("/api/projects", &bob, json!({ "project": bad })).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r.body["message"].as_str().unwrap().contains("duration"), "{:?}", r.body);
    assert_eq!(t.post("/api/projects", &bob, json!({ "project": { "nope": 1 } })).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(t.get("/api/projects", &bob).await.body.as_array().unwrap().len(), 1);

    // Sharing: the owner needs project.share; a creator has none.
    let share = format!("{url}/shares");
    assert_eq!(t.post(&share, &carol, json!({ "username": "bob", "access": "read" })).await.status, StatusCode::FORBIDDEN);
    assert_eq!(t.post(&share, &bob, json!({ "username": "nobody", "access": "read" })).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(t.post(&share, &bob, json!({ "username": "bob", "access": "read" })).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(t.post(&share, &bob, json!({ "username": "carol", "access": "admin" })).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = t.post(&share, &bob, json!({ "username": "carol", "access": "read" })).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body[0]["username"], "carol");

    // Read access opens it and nothing more.
    let r = t.get(&url, &carol).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["access"], "read");

    assert_eq!(patch(&t, &carol, &url, json!({ "name": "Mine now" })).await.status, StatusCode::FORBIDDEN);
    assert_eq!(t.call(Method::DELETE, &url, &carol, None).await.status, StatusCode::FORBIDDEN);
    assert_eq!(t.get(&share, &carol).await.status, StatusCode::FORBIDDEN, "creator lacks project.share");

    // Write access edits, with optimistic concurrency.
    t.post(&share, &bob, json!({ "username": "carol", "access": "write" })).await;
    let r = patch(&t, &carol, &url, json!({ "name": "Renamed", "revision": 1 })).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["name"], "Renamed");
    assert_eq!(r.body["revision"], 2);
    assert_eq!(r.body["project"]["name"], "Renamed");
    let stale = patch(&t, &bob, &url, json!({ "name": "Stale", "revision": 1 })).await;
    assert_eq!(stale.status, StatusCode::CONFLICT, "{:?}", stale.body);
    let mut edited = r.body["project"].clone();
    edited["tracks"][0]["clips"][0]["duration"] = json!(1.25);
    let r = patch(&t, &bob, &url, json!({ "project": edited, "revision": 2 })).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["duration"], 1.25);
    // Writers cannot delete or re-share.
    assert_eq!(t.call(Method::DELETE, &url, &carol, None).await.status, StatusCode::FORBIDDEN);
    let mine = t.get("/api/projects", &carol).await;
    assert_eq!(mine.body[0]["access"], "write");

    // The guest role reads projects but cannot make them or preview.
    assert_eq!(t.get("/api/projects", &guest).await.status, StatusCode::OK);
    assert_eq!(t.post("/api/projects", &guest, json!({ "project": doc("G") })).await.status, StatusCode::FORBIDDEN);
    assert_eq!(t.post("/api/preview", &guest, json!({ "project": doc("G") })).await.status, StatusCode::FORBIDDEN);

    // Unsharing closes it again; deleting is the owner's.
    let r = t.call(Method::DELETE, &format!("{share}/carol"), &bob, None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(t.get(&url, &carol).await.status, StatusCode::NOT_FOUND);
    assert_eq!(t.call(Method::DELETE, &format!("{share}/carol"), &bob, None).await.status, StatusCode::NOT_FOUND);
    assert_eq!(t.call(Method::DELETE, &url, &bob, None).await.status, StatusCode::NO_CONTENT);
    assert_eq!(t.get(&url, &bob).await.status, StatusCode::NOT_FOUND);

    // Shares and deletes leave an audit trail.
    let audit = t.get("/api/admin/audit?limit=50", &admin).await;
    let actions: Vec<&str> = audit.body.as_array().unwrap().iter().filter_map(|e| e["action"].as_str()).collect();
    for a in ["project.share", "project.unshare", "project.delete"] {
        assert!(actions.contains(&a), "no {a} in {actions:?}");
    }
}

#[tokio::test]
async fn a_token_without_project_permissions_cannot_touch_projects_and_none_is_401() {
    let t = TestApp::new().await;
    let admin = t.setup_admin().await;
    assert_eq!(t.get("/api/projects", &Auth::None).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(t.post("/api/preview", &Auth::None, json!({})).await.status, StatusCode::UNAUTHORIZED);
    let r = t.post("/api/tokens", &admin, json!({ "name": "t", "permissions": ["corpus.read"] })).await;
    let bearer = Auth::Bearer(r.body["secret"].as_str().unwrap().to_string());
    assert_eq!(t.get("/api/projects", &bearer).await.status, StatusCode::FORBIDDEN);
    let r = t.post("/api/tokens", &admin, json!({ "name": "p", "permissions": ["project.read", "project.write"] })).await;
    let bearer = Auth::Bearer(r.body["secret"].as_str().unwrap().to_string());
    assert_eq!(t.post("/api/projects", &bearer, json!({ "project": doc("Via token") })).await.status, StatusCode::OK);
    assert_eq!(t.post("/api/projects/1/shares", &bearer, json!({ "username": "x", "access": "read" })).await.status, StatusCode::FORBIDDEN);
}

// ── With media ───────────────────────────────────────────────────────────────

const WORDS: [&str; 20] = [
    "oh", "no", "go", "you", "me", "ah", "boom", "kiss", "see", "yes", "hello", "world", "bum", "dog", "cat", "tea", "top",
    "low", "more", "now",
];

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

fn make_source(path: &Path) {
    let st = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=480x270:rate=25:duration=12"])
        .args(["-f", "lavfi", "-i", "aevalsrc='0.5*lt(mod(t,0.6),0.5)*sin(2*PI*t*(180+15*floor(t/0.6)))':s=44100:d=12"])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-g", "50", "-c:a", "aac", "-shortest"])
        .arg(path)
        .status()
        .expect("ffmpeg runs");
    assert!(st.success(), "could not make the test source");
}

async fn make_music_bundle(work: &Path) -> Vec<u8> {
    let src = work.join("v1.mp4");
    make_source(&src);
    let db = work.join("music-corpus.db");
    let _ = std::fs::remove_file(&db);
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
        "INSERT INTO sources (video_id, source_file, title) VALUES ('v1', 'downloads/v1.mp4', 'sine')",
    ] {
        sqlx::query(sql).execute(&mut conn).await.unwrap();
    }
    for (k, w) in WORDS.iter().enumerate() {
        let start = k as f64 * 0.6;
        sqlx::query("INSERT INTO word_clips (source_id, word, start_time, end_time, source_file) VALUES (1, ?, ?, ?, 'downloads/v1.mp4')")
            .bind(*w)
            .bind(start)
            .bind(start + 0.45)
            .execute(&mut conn)
            .await
            .unwrap();
    }
    conn.close().await.unwrap();
    let mut b = tar::Builder::new(Vec::new());
    raw_entry(&mut b, "corpus.db", tar::EntryType::Regular, &std::fs::read(&db).unwrap(), None);
    raw_entry(&mut b, "downloads/", tar::EntryType::Directory, b"", None);
    raw_entry(&mut b, "downloads/v1.mp4", tar::EntryType::Regular, &std::fs::read(&src).unwrap(), None);
    zstd::encode_all(&b.into_inner().unwrap()[..], 3).unwrap()
}

fn vlq(mut v: u32) -> Vec<u8> {
    let mut out = vec![(v & 0x7F) as u8];
    v >>= 7;
    while v > 0 {
        out.push(((v & 0x7F) as u8) | 0x80);
        v >>= 7;
    }
    out.reverse();
    out
}

fn track(name: &str, channel: u8, program: Option<u8>, notes: &[(u32, u32, u8)]) -> Vec<u8> {
    let mut ev: Vec<(u32, u8, u8, u8)> = Vec::new();
    for &(t, len, key) in notes {
        ev.push((t, 0x90 | channel, key, 96));
        ev.push((t + len, 0x80 | channel, key, 0));
    }
    ev.sort_by_key(|e| (e.0, e.1 & 0xF0 == 0x90));
    let mut body = vec![0x00, 0xFF, 0x03, name.len() as u8];
    body.extend_from_slice(name.as_bytes());
    if let Some(p) = program {
        body.extend_from_slice(&[0x00, 0xC0 | channel, p]);
    }
    let mut last = 0;
    for (t, s, a, b) in ev {
        body.extend(vlq(t - last));
        body.extend_from_slice(&[s, a, b]);
        last = t;
    }
    body.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]);
    let mut out = b"MTrk".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend(body);
    out
}

fn song_midi() -> Vec<u8> {
    let beat = 480;
    let mut tempo = vec![0x00, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20, 0x00, 0xFF, 0x2F, 0x00];
    let mut t0 = b"MTrk".to_vec();
    t0.extend_from_slice(&(tempo.len() as u32).to_be_bytes());
    t0.append(&mut tempo);
    let lead: Vec<(u32, u32, u8)> = (0..16).map(|i| (i * beat / 2, beat / 2 - 20, [72, 74, 76, 79][i as usize % 4])).collect();
    let bass: Vec<(u32, u32, u8)> = (0..8).map(|i| (i * beat, beat - 30, [36, 36, 43, 41][i as usize % 4])).collect();
    let mut drums: Vec<(u32, u32, u8)> = Vec::new();
    for bar in 0..4 {
        for b in 0..4 {
            let t = (bar * 4 + b) * beat;
            drums.push((t, 60, if b % 2 == 0 { 36 } else { 38 }));
            drums.push((t + beat / 2, 40, 42));
        }
    }
    let tracks = [t0, track("Lead", 0, Some(80), &lead), track("Bass", 1, Some(33), &bass), track("Kit", 9, None, &drums)];
    let mut out = b"MThd".to_vec();
    out.extend_from_slice(&6u32.to_be_bytes());
    out.extend_from_slice(&[0, 1, 0, tracks.len() as u8, (beat >> 8) as u8, (beat & 0xFF) as u8]);
    for t in tracks {
        out.extend(t);
    }
    out
}

struct Raw {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

async fn raw(t: &TestApp, method: Method, uri: &str, auth: &Auth, body: Option<Value>) -> Raw {
    let b = with_auth(Request::builder().method(method).uri(uri), auth);
    let req = match body {
        Some(v) => b.header(header::CONTENT_TYPE, "application/json").body(Body::from(v.to_string())).unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let res = t.app.clone().oneshot(req).await.unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    Raw { status, headers, body }
}

fn probe_bytes(dir: &Path, name: &str, bytes: &[u8]) -> ytp_encode::Probe {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    ytp_encode::Encoder::new("ffmpeg").probe(&path).expect("a playable file")
}

async fn wait_job(t: &TestApp, auth: &Auth, id: &str) -> Value {
    for _ in 0..1800 {
        let r = t.get(&format!("/api/jobs/{id}"), auth).await;
        assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
        if ["done", "failed", "cancelled"].contains(&r.body["state"].as_str().unwrap()) {
            return r.body;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("job {id} never finished");
}

async fn setup() -> (TestApp, Auth) {
    let t = TestApp::new().await;
    let admin = t.setup_admin().await;
    let bundle = make_music_bundle(t.dir.path()).await;
    let r = t.install(&admin, &[("file", Some("music.tar.zst"), &bundle)]).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    (t, admin)
}

fn layout(p: &ytp_encode::Probe) -> Vec<(String, String, u32, u32)> {
    p.streams.iter().map(|s| (s.kind.clone(), s.codec.clone(), s.width, s.height)).collect()
}

#[tokio::test]
async fn a_sentence_plan_becomes_a_project_that_previews_and_renders_like_the_job() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg is not on PATH; skipping");
        return;
    }
    let (t, admin) = setup().await;
    let body = json!({ "text": "hello world, oh no", "voice": "music", "options": { "seed": "42", "speed": 1.1 } });

    // The plan, without rendering anything.
    let t0 = Instant::now();
    let r = t.post("/api/ytp/plan", &admin, body.clone()).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    eprintln!("sentence plan: {:?}", t0.elapsed());
    let project = r.body["project"].clone();
    assert_eq!(project["generator"]["kind"], "ytp");
    assert_eq!(project["generator"]["seed"], "42");
    let clips = project["tracks"][0]["clips"].as_array().unwrap();
    assert!(clips.len() >= 2, "{clips:?}");
    assert_eq!(clips[0]["start"], 0.0);
    assert_eq!(clips[0]["source"]["type"], "corpus_clip");
    assert_eq!(clips[0]["source"]["corpus"], "music");
    assert!(clips[0]["source"]["file"].as_str().unwrap().ends_with("v1.mp4"));
    assert!(clips[0]["effects"].as_array().unwrap().iter().any(|e| e["type"] == "speed"));
    assert!(!r.body["report"]["found"].as_array().unwrap().is_empty());
    assert_eq!(t.get("/api/projects", &admin).await.body, json!([]), "nothing was saved");
    // The same seed plans the same thing.
    let again = t.post("/api/ytp/plan", &admin, body.clone()).await;
    assert_eq!(again.body["project"], project);

    // Saved as a project of the caller's.
    let mut save = body.clone();
    save["save"] = json!(true);
    let r = t.post("/api/ytp/plan", &admin, save).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let id = r.body["id"].as_i64().unwrap();
    assert_eq!(r.body["saved"]["project"]["tracks"], project["tracks"]);

    // A rendering of the project is the video the same request makes as a job.
    let job = t.post("/api/ytp/generate", &admin, body.clone()).await;
    let want = wait_job(&t, &admin, job.body["job_id"].as_str().unwrap()).await;
    assert_eq!(want["state"], "done", "{want:?}");
    let r = t.post(&format!("/api/projects/{id}/render"), &admin, json!({})).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let got = wait_job(&t, &admin, r.body["job_id"].as_str().unwrap()).await;
    assert_eq!(got["state"], "done", "{got:?}");
    assert_eq!(got["kind"], "project");
    let (dw, dg) = (want["duration"].as_f64().unwrap(), got["duration"].as_f64().unwrap());
    assert!((dw - dg).abs() < 0.05, "job {dw} vs project {dg}");
    let fetch = |job: &Value| {
        let uri = format!("/api/jobs/{}/result", job["id"].as_str().unwrap());
        let (t, admin) = (t.app.clone(), admin.clone());
        async move {
            let req = with_auth(Request::builder().uri(uri), &admin).body(Body::empty()).unwrap();
            t.oneshot(req).await.unwrap().into_body().collect().await.unwrap().to_bytes().to_vec()
        }
    };
    let (bw, bg) = (fetch(&want).await, fetch(&got).await);
    let (pw, pg) = (probe_bytes(t.dir.path(), "want.mp4", &bw), probe_bytes(t.dir.path(), "got.mp4", &bg));
    assert_eq!(layout(&pw), layout(&pg));
    assert!((pw.duration - pg.duration).abs() < 0.1, "{} vs {}", pw.duration, pg.duration);
    eprintln!("job {} bytes, project render {} bytes, identical: {}", bw.len(), bg.len(), bw == bg);

    // The preview of a window: a small MP4 of the asked length, cached the second time.
    let mut times = Vec::new();
    for _ in 0..2 {
        let t1 = Instant::now();
        let r = raw(&t, Method::POST, "/api/preview", &admin, Some(json!({ "project_id": id, "start": 0.5, "duration": 2.0 }))).await;
        times.push(t1.elapsed());
        assert_eq!(r.status, StatusCode::OK, "{}", String::from_utf8_lossy(&r.body[..r.body.len().min(300)]));
        assert_eq!(r.headers[header::CONTENT_TYPE], "video/mp4");
        let p = probe_bytes(t.dir.path(), "preview.mp4", &r.body);
        assert!((p.duration - 2.0).abs() < 0.25, "preview is {} s", p.duration);
        assert!(p.has_video && p.has_audio);
        let info: Value = serde_json::from_str(r.headers["x-preview-info"].to_str().unwrap()).unwrap();
        assert_eq!(info["kind"], "sequence");
        if times.len() == 1 {
            assert_eq!(r.headers["x-preview-cache"], "miss");
            assert!(info["clips"].as_u64().unwrap() >= 1);
        } else {
            assert_eq!(r.headers["x-preview-cache"], "hit");
        }
    }
    eprintln!("preview 2 s: first {:?}, cached {:?}", times[0], times[1]);
    // An inline project (as the editor sends it) works too; bad windows do not.
    let r = raw(&t, Method::POST, "/api/preview", &admin, Some(json!({ "project": project, "start": 0.0, "duration": 1.0 }))).await;
    assert_eq!(r.status, StatusCode::OK);
    let r = raw(&t, Method::POST, "/api/preview", &admin, Some(json!({ "project_id": id, "start": 500.0, "duration": 1.0 }))).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = raw(&t, Method::POST, "/api/preview", &admin, Some(json!({ "project_id": id, "project": project }))).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = raw(&t, Method::POST, "/api/preview", &admin, Some(json!({ "project_id": 424242 }))).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // Others can neither preview nor render what is not theirs.
    let carol = t.create_user(&admin, "carol", &["editor"]).await;
    let r = raw(&t, Method::POST, "/api/preview", &carol, Some(json!({ "project_id": id }))).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(t.post(&format!("/api/projects/{id}/render"), &carol, json!({})).await.status, StatusCode::NOT_FOUND);
    // ...until it is shared: then a sharee renders it as their own job.
    t.post(&format!("/api/projects/{id}/shares"), &admin, json!({ "username": "carol", "access": "read" })).await;
    let r = raw(&t, Method::POST, "/api/preview", &carol, Some(json!({ "project_id": id, "start": 0.0, "duration": 1.0 }))).await;
    assert_eq!(r.status, StatusCode::OK);
    let r = t.post(&format!("/api/projects/{id}/render"), &carol, json!({})).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(wait_job(&t, &carol, r.body["job_id"].as_str().unwrap()).await["state"], "done");
}

#[tokio::test]
async fn a_planned_sentence_with_a_project_edit_renders_the_edit() {
    if !have_ffmpeg() {
        return;
    }
    let (t, admin) = setup().await;
    let r = t.post("/api/ytp/plan", &admin, json!({ "text": "hello world", "voice": "music", "options": { "seed": "3" }, "save": true })).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let id = r.body["id"].as_i64().unwrap();
    let mut project = r.body["saved"]["project"].clone();
    let before = r.body["saved"]["duration"].as_f64().unwrap();
    // Stretch the first clip and make room for it.
    let clips = project["tracks"][0]["clips"].as_array_mut().unwrap();
    let old = clips[0]["duration"].as_f64().unwrap();
    clips[0]["duration"] = json!(old + 1.0);
    clips[0]["effects"].as_array_mut().unwrap().push(json!({ "type": "stretch", "factor": 3.0 }));
    for c in clips.iter_mut().skip(1) {
        c["start"] = json!(c["start"].as_f64().unwrap() + 1.0);
    }
    let r = t.call(Method::PATCH, &format!("/api/projects/{id}"), &admin, Some(json!({ "project": project, "revision": 1 }))).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert!((r.body["duration"].as_f64().unwrap() - (before + 1.0)).abs() < 1e-6);
    let job = t.post(&format!("/api/projects/{id}/render"), &admin, json!({})).await;
    let done = wait_job(&t, &admin, job.body["job_id"].as_str().unwrap()).await;
    assert_eq!(done["state"], "done", "{done:?}");
    assert!((done["duration"].as_f64().unwrap() - (before + 1.0)).abs() < 0.05);
}

#[tokio::test]
async fn a_song_plan_has_a_track_per_part_and_plays_through_the_song_renderer() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg is not on PATH; skipping");
        return;
    }
    let (t, admin) = setup().await;
    let (ct, body) = multipart(&[("midi", Some("Test Song.mid"), &song_midi())]);
    let req = with_auth(Request::builder().method(Method::POST).uri("/api/ytpmv/midi").header(header::CONTENT_TYPE, ct), &admin)
        .body(Body::from(body))
        .unwrap();
    let up = t.send(req).await;
    assert_eq!(up.status, StatusCode::OK, "{:?}", up.body);
    let midi_id = up.body["midi_id"].as_str().unwrap().to_string();
    let parts: Vec<Value> = up.body["parts"].as_array().unwrap().iter().map(|p| p["settings"].clone()).collect();

    let request = json!({ "midi_id": midi_id, "parts": parts, "options": { "max_seconds": 6 }, "voice": "music" });
    let t0 = Instant::now();
    let r = t.post("/api/ytpmv/plan", &admin, request.clone()).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    eprintln!("song plan: {:?}", t0.elapsed());
    let project = r.body["project"].clone();
    assert_eq!(project["generator"]["kind"], "ytpmv");
    assert_eq!(project["generator"]["request"]["midi_id"], midi_id.as_str());
    let tracks = project["tracks"].as_array().unwrap();
    assert!(tracks.len() >= 2, "{tracks:?}");
    for tr in tracks {
        assert_eq!(tr["generated"], true);
        assert!(tr["part"].is_string());
        assert!(!tr["clips"].as_array().unwrap().is_empty(), "{tr:?}");
    }
    let first = &tracks[0]["clips"][0];
    assert!(first["start"].as_f64().unwrap() >= 0.0 && first["duration"].as_f64().unwrap() > 0.0);
    assert_eq!(first["source"]["type"], "corpus_clip");
    assert!(first["source"]["word"].is_string());

    let mut save = request.clone();
    save["save"] = json!(true);
    let r = t.post("/api/ytpmv/plan", &admin, save).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let id = r.body["id"].as_i64().unwrap();
    assert_eq!(r.body["saved"]["kind"], "song");

    // Preview of a window: its picture and its sound.
    let mut last = Value::Null;
    for (start, dur) in [(0.0, 3.0), (2.0, 3.0)] {
        let t1 = Instant::now();
        let r = raw(&t, Method::POST, "/api/preview", &admin, Some(json!({ "project_id": id, "start": start, "duration": dur }))).await;
        assert_eq!(r.status, StatusCode::OK, "{}", String::from_utf8_lossy(&r.body[..r.body.len().min(400)]));
        eprintln!("song preview {dur} s at {start}: {:?}", t1.elapsed());
        let p = probe_bytes(t.dir.path(), "song-preview.mp4", &r.body);
        assert!(p.has_video && p.has_audio);
        assert!((p.duration - dur).abs() < 0.3, "{} vs {dur}", p.duration);
        last = serde_json::from_str(r.headers["x-preview-info"].to_str().unwrap()).unwrap();
    }
    assert_eq!(last["kind"], "song");

    // Muting a track changes what the window plays (a different preview), and
    // the project still renders as the song.
    let mut edited = r_project(&t, &admin, id).await;
    edited["tracks"][0]["mute"] = json!(true);
    let r = t.call(Method::PATCH, &format!("/api/projects/{id}"), &admin, Some(json!({ "project": edited }))).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let r = raw(&t, Method::POST, "/api/preview", &admin, Some(json!({ "project_id": id, "start": 0.0, "duration": 3.0 }))).await;
    assert_eq!(r.headers["x-preview-cache"], "miss", "a new mixer is a new preview");

    let job = t.post(&format!("/api/projects/{id}/render"), &admin, json!({})).await;
    assert_eq!(job.status, StatusCode::OK, "{:?}", job.body);
    let done = wait_job(&t, &admin, job.body["job_id"].as_str().unwrap()).await;
    assert_eq!(done["state"], "done", "{done:?}");
    let want = project["tracks"].as_array().unwrap().iter().flat_map(|tr| tr["clips"].as_array().unwrap()).map(|c| c["start"].as_f64().unwrap() + c["duration"].as_f64().unwrap()).fold(0.0, f64::max);
    assert!(done["duration"].as_f64().unwrap() >= want - 0.1, "{} vs plan {want}", done["duration"]);
}

async fn r_project(t: &TestApp, auth: &Auth, id: i64) -> Value {
    t.get(&format!("/api/projects/{id}"), auth).await.body["project"].clone()
}
