//! YTPMV end to end with a real ffmpeg: a synthetic corpus (a sine "voice"
//! saying a word every 0.6 s), a small MIDI file, and the whole chain —
//! upload → parts → preview-mix → render → video.

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

const WORDS: [&str; 20] = [
    "oh", "no", "go", "you", "me", "ah", "boom", "kiss", "see", "yes", "hello", "world", "bum", "dog", "cat", "tea", "top",
    "low", "more", "now",
];

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// 12 s of test pattern and a sine voice: 0.5 s of tone in each 0.6 s slot,
/// each slot a different pitch.
fn make_source(path: &Path) {
    let st = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=480x270:rate=25:duration=12"])
        .args(["-f", "lavfi", "-i", "aevalsrc='0.5*lt(mod(t,0.6),0.5)*sin(2*PI*t*(180+15*floor(t/0.6)))':s=44100:d=12"])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
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
        sqlx::query(
            "INSERT INTO word_clips (source_id, word, start_time, end_time, source_file) VALUES (1, ?, ?, ?, 'downloads/v1.mp4')",
        )
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

// ── A small MIDI file: lead, bass, and a kick/snare/hat kit; 4 bars at 120 BPM ──

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

/// A track of `(start tick, length ticks, key)` notes on `channel`.
fn track(name: &str, channel: u8, program: Option<u8>, notes: &[(u32, u32, u8)]) -> Vec<u8> {
    let mut ev: Vec<(u32, u8, u8, u8)> = Vec::new(); // tick, status, a, b
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

// ── Plumbing ──

struct Raw {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

async fn raw(t: &TestApp, req: Request<Body>) -> Raw {
    let res = t.app.clone().oneshot(req).await.unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    Raw { status, headers, body }
}

async fn post_raw(t: &TestApp, auth: &Auth, uri: &str, body: Value) -> Raw {
    let req = with_auth(Request::builder().method(Method::POST).uri(uri).header(header::CONTENT_TYPE, "application/json"), auth)
        .body(Body::from(body.to_string()))
        .unwrap();
    raw(t, req).await
}

async fn upload(t: &TestApp, auth: &Auth, name: &str, data: &[u8]) -> Resp {
    let (ct, body) = multipart(&[("midi", Some(name), data)]);
    let req = with_auth(Request::builder().method(Method::POST).uri("/api/ytpmv/midi").header(header::CONTENT_TYPE, ct), auth)
        .body(Body::from(body))
        .unwrap();
    t.send(req).await
}

fn wav_samples(bytes: &[u8]) -> (u32, u16, Vec<i16>) {
    let w = ytp_music::wav::read(bytes).expect("a 16-bit wav");
    (w.sample_rate, w.channels, w.samples)
}

fn info(r: &Raw) -> Value {
    serde_json::from_str(r.headers["x-ytpmv-info"].to_str().unwrap()).expect("info header is JSON")
}

async fn wait_job(t: &TestApp, auth: &Auth, id: &str) -> Value {
    for _ in 0..1200 {
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

#[tokio::test]
async fn upload_parts_preview_mix_and_render() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg is not on PATH; skipping");
        return;
    }
    let (t, admin) = setup().await;

    // Upload: the file is read and every part gets a suggested sound.
    let t0 = Instant::now();
    let r = upload(&t, &admin, "Test Song.mid", &song_midi()).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    eprintln!("upload + first analysis (measuring the corpus): {:?}", t0.elapsed());
    let midi_id = r.body["midi_id"].as_str().unwrap().to_string();
    assert_eq!(midi_id.len(), 12);
    assert_eq!(r.body["title"], "Test Song");
    assert_eq!(r.body["corpus"], "music");
    let parts = r.body["parts"].as_array().unwrap().clone();
    // lead + bass + kick/snare/hats
    assert_eq!(parts.len(), 5, "{parts:?}");
    for p in &parts {
        assert!(p["recommended"]["text"].is_string(), "every part got a word: {p}");
        assert!(p["settings"]["text"].is_string());
        assert!(p["chip_tones"]["nes"].is_string());
    }
    assert!(r.body["instruments"]["machines"]["nes"]["melodic"].is_array());
    assert!(r.body["program_tones"].is_object());

    // The library lists it; another user sees nothing of it.
    let lib = t.get("/api/ytpmv/library", &admin).await;
    assert_eq!(lib.body.as_array().unwrap().len(), 1);
    assert_eq!(lib.body[0]["midi_id"], midi_id.as_str());
    assert_eq!(lib.body[0]["parts"], 2);
    assert_eq!(lib.body[0]["drums"], 3);
    let carol = t.create_user(&admin, "carol", &["creator"]).await;
    assert_eq!(t.get("/api/ytpmv/library", &carol).await.body, json!([]));
    assert_eq!(t.get(&format!("/api/ytpmv/midi/{midi_id}"), &carol).await.status, StatusCode::NOT_FOUND);

    // Notes for the timeline.
    let ids: Vec<String> = parts.iter().map(|p| p["id"].as_str().unwrap().to_string()).collect();
    let n = t.get(&format!("/api/ytpmv/midi/{midi_id}/notes?start=0&end=1&parts={}", ids[0]), &admin).await;
    assert_eq!(n.status, StatusCode::OK, "{:?}", n.body);
    assert!(n.body["parts"][&ids[0]].as_array().unwrap().len() >= 2);
    let bad = t.get(&format!("/api/ytpmv/midi/{midi_id}/notes?start=5&end=1&parts={}", ids[0]), &admin).await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);

    // One part again for another voice.
    let pv = t.get(&format!("/api/ytpmv/midi/{midi_id}/part/{}?corpus=music", ids[0]), &admin).await;
    assert_eq!(pv.status, StatusCode::OK, "{:?}", pv.body);
    assert_eq!(pv.body["corpus"], "music");

    // A sample: a word, and a phrase through the sentence machinery.
    let s = t.post("/api/ytpmv/sample", &admin, json!({ "text": "Oh", "take": 0 })).await;
    assert_eq!(s.status, StatusCode::OK, "{:?}", s.body);
    assert!(s.body["f0"].as_f64().unwrap() > 100.0, "the sine voice has a pitch: {}", s.body);
    let phrase = t.post("/api/ytpmv/sample", &admin, json!({ "text": "hello world" })).await;
    assert_eq!(phrase.status, StatusCode::OK, "{:?}", phrase.body);
    assert!(phrase.body["duration"].as_f64().unwrap() > 0.3);
    let nope = t.post("/api/ytpmv/sample", &admin, json!({ "text": "zzzzqqq" })).await;
    assert_eq!(nope.status, StatusCode::UNPROCESSABLE_ENTITY);
    let a = post_raw(&t, &admin, "/api/ytpmv/sample/audio", json!({ "text": "oh" })).await;
    assert_eq!(a.status, StatusCode::OK);
    assert_eq!(a.headers[header::CONTENT_TYPE], "audio/wav");
    assert_eq!(wav_samples(&a.body).1, 1);

    // The fast audio-only mix of the whole song, as the page sends it.
    let settings: Vec<Value> = parts.iter().map(|p| p["settings"].clone()).collect();
    let body = json!({ "midi_id": midi_id, "parts": settings, "options": { "vary": "rotate" }, "start": 0, "seconds": 10 });
    let t1 = Instant::now();
    let m = post_raw(&t, &admin, "/api/ytpmv/preview-mix", body.clone()).await;
    let cold = t1.elapsed();
    assert_eq!(m.status, StatusCode::OK, "{}", String::from_utf8_lossy(&m.body));
    assert_eq!(m.headers[header::CONTENT_TYPE], "audio/wav");
    let (rate, channels, samples) = wav_samples(&m.body);
    assert_eq!((rate, channels), (44100, 2));
    assert!(samples.len() > 44100 * 2 * 3, "about the song's length: {}", samples.len());
    let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
    assert!(peak > 3000, "the mix is not silent: {peak}");
    let i = info(&m);
    assert!(i["duration"].as_f64().unwrap() > 3.0);
    assert!(i["parts"].as_array().unwrap().len() >= 4);
    let t2 = Instant::now();
    let m2 = post_raw(&t, &admin, "/api/ytpmv/preview-mix", body).await;
    let warm = t2.elapsed();
    assert_eq!(m2.body, m.body, "a render is the same twice");
    eprintln!("preview-mix: first {cold:?}, warm {warm:?}, {} s of audio", i["duration"]);
    assert!(warm < Duration::from_secs(5), "preview-mix should be quick: {warm:?}");

    // One part, and an instrument audition of a single key.
    let one = post_raw(&t, &admin, "/api/ytpmv/preview", json!({ "midi_id": midi_id, "part": settings[0], "seconds": 3 })).await;
    assert_eq!(one.status, StatusCode::OK, "{}", String::from_utf8_lossy(&one.body));
    let key = post_raw(
        &t,
        &admin,
        "/api/ytpmv/preview-mix",
        json!({ "midi_id": midi_id, "parts": settings, "part_id": ids[0], "notes": [60, 64], "options": {} }),
    )
    .await;
    assert_eq!(key.status, StatusCode::OK, "{}", String::from_utf8_lossy(&key.body));
    assert_eq!(info(&key)["keyboard"].as_array().unwrap().len(), 2);
    // The whole song through the NES, with no voice at all.
    let nes = post_raw(
        &t,
        &admin,
        "/api/ytpmv/preview-mix",
        json!({ "midi_id": midi_id, "parts": [], "options": { "chip": "nes", "synth": true }, "seconds": 4 }),
    )
    .await;
    assert_eq!(nes.status, StatusCode::OK, "{}", String::from_utf8_lossy(&nes.body));
    assert_ne!(nes.body, m.body);
    // Bad requests.
    let r = t.post("/api/ytpmv/preview-mix", &admin, json!({ "midi_id": midi_id, "note": 300, "part_id": ids[0] })).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{:?}", r.body);
    let r = t.post("/api/ytpmv/preview-mix", &admin, json!({ "midi_id": "zzzzzzzzzzzz" })).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // The render: a job that ends in a video.
    let r = t.post("/api/ytpmv/render", &admin, json!({ "midi_id": midi_id, "parts": settings, "options": { "draft": true, "max_seconds": 4 } })).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let job_id = r.body["job_id"].as_str().unwrap().to_string();
    let job = wait_job(&t, &admin, &job_id).await;
    assert_eq!(job["state"], "done", "{job}");
    assert_eq!(job["kind"], "ytpmv");
    assert_eq!(job["progress"], 1.0);
    assert!(job["duration"].as_f64().unwrap() > 3.0);
    assert_eq!(job["report"]["size"], json!([1920, 1080]));
    assert!(job["report"]["parts"].as_array().unwrap().len() == 5);
    let video = raw(
        &t,
        with_auth(Request::builder().method(Method::GET).uri(format!("/api/jobs/{job_id}/result")), &admin).body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(video.status, StatusCode::OK);
    assert_eq!(video.headers[header::CONTENT_TYPE], "video/mp4");
    assert!(video.body.len() > 10_000, "{} bytes", video.body.len());
    assert_eq!(&video.body[4..8], b"ftyp");
    let probe = t.dir.path().join("out.mp4");
    std::fs::write(&probe, &video.body).unwrap();
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream=codec_type,width,height", "-of", "json"])
        .arg(&probe)
        .output()
        .expect("ffprobe runs");
    let streams: Value = serde_json::from_slice(&out.stdout).unwrap();
    let kinds: Vec<&str> = streams["streams"].as_array().unwrap().iter().map(|s| s["codec_type"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"video") && kinds.contains(&"audio"), "{streams}");
    let v = streams["streams"].as_array().unwrap().iter().find(|s| s["codec_type"] == "video").unwrap();
    assert_eq!((v["width"].as_i64(), v["height"].as_i64()), (Some(1920), Some(1080)));

    // A MIDI in use by a running render cannot be deleted; afterwards it can.
    let del = t.call(Method::DELETE, &format!("/api/ytpmv/midi/{midi_id}"), &admin, None).await;
    assert_eq!(del.status, StatusCode::NO_CONTENT);
    assert_eq!(t.get("/api/ytpmv/library", &admin).await.body, json!([]));
}

#[tokio::test]
async fn a_song_with_no_voice_renders_oscilloscopes() {
    if !have_ffmpeg() {
        return;
    }
    // No corpus installed at all: the chip plays and every part is a scope.
    let t = TestApp::new().await;
    let admin = t.setup_admin().await;
    let r = upload(&t, &admin, "scopes.mid", &song_midi()).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let midi_id = r.body["midi_id"].as_str().unwrap().to_string();
    assert_eq!(r.body["corpus"], "");
    assert!(r.body["parts"][0]["recommended"]["text"].is_null());

    let r = t
        .post(
            "/api/ytpmv/render",
            &admin,
            json!({ "midi_id": midi_id, "options": { "synth": true, "chip": "gb", "draft": true, "max_seconds": 3, "layout": "classic" } }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let job = wait_job(&t, &admin, r.body["job_id"].as_str().unwrap()).await;
    assert_eq!(job["state"], "done", "{job}");
    assert_eq!(job["report"]["options"]["screen"], "gb");
    assert_eq!(job["report"]["options"]["synth"], true);
}

#[tokio::test]
async fn permissions_and_limits() {
    let t = TestApp::new().await;
    let admin = t.setup_admin().await;
    let guest = t.create_user(&admin, "gina", &["guest"]).await;
    // A guest cannot touch any of it.
    assert_eq!(t.get("/api/ytpmv/library", &guest).await.status, StatusCode::FORBIDDEN);
    assert_eq!(upload(&t, &guest, "x.mid", &song_midi()).await.status, StatusCode::FORBIDDEN);

    // A role with generate.ytpmv but no export.render may upload, preview, not render.
    let r = t
        .post(
            "/api/admin/roles",
            &admin,
            json!({ "name": "listener", "permissions": ["generate.ytpmv", "corpus.read"],
                    "quota": { "max_concurrent_jobs": 1, "max_render_seconds": 60, "max_height": 480, "requests_per_minute": 600 } }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let lis = t.create_user(&admin, "lou", &["listener"]).await;
    let up = upload(&t, &lis, "a.mid", &song_midi()).await;
    assert_eq!(up.status, StatusCode::OK, "{:?}", up.body);
    let id = up.body["midi_id"].as_str().unwrap();
    let r = t.post("/api/ytpmv/render", &lis, json!({ "midi_id": id, "options": { "synth": true } })).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{:?}", r.body);

    // Files that are not MIDI, empty or too big are refused; the library
    // does not keep them.
    assert_eq!(upload(&t, &lis, "a.mid", b"not midi").await.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(upload(&t, &lis, "a.mid", b"").await.status, StatusCode::UNPROCESSABLE_ENTITY);
    let mut big = song_midi();
    big.resize(3 << 20, 0);
    let r = upload(&t, &lis, "big.mid", &big).await;
    // Past the body limit the multipart stream is cut off: 400 (`too_large` in the contract).
    assert!(r.status == StatusCode::UNPROCESSABLE_ENTITY || r.status == StatusCode::BAD_REQUEST, "{:?} {:?}", r.status, r.body);
    assert_eq!(t.get("/api/ytpmv/library", &lis).await.body.as_array().unwrap().len(), 1);

    // Rename.
    let r = t.call(Method::PATCH, &format!("/api/ytpmv/midi/{id}"), &lis, Some(json!({ "title": "Renamed" }))).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(t.get("/api/ytpmv/library", &lis).await.body[0]["title"], "Renamed");
    let r = t.call(Method::PATCH, &format!("/api/ytpmv/midi/{id}"), &lis, Some(json!({ "title": "  " }))).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);

    // A render longer than the role's limit fails the job, not the server.
    let r = t
        .call(
            Method::POST,
            "/api/admin/roles",
            &admin,
            Some(json!({ "name": "shorty", "permissions": ["generate.ytpmv", "export.render"],
                         "quota": { "max_concurrent_jobs": 1, "max_render_seconds": 1, "max_height": 480, "requests_per_minute": 600 } })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let sh = t.create_user(&admin, "sho", &["shorty"]).await;
    let id = upload(&t, &sh, "b.mid", &song_midi()).await.body["midi_id"].as_str().unwrap().to_string();
    let r = t.post("/api/ytpmv/render", &sh, json!({ "midi_id": id, "options": { "synth": true, "chip": "nes" } })).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    if !have_ffmpeg() {
        return;
    }
    let job = wait_job(&t, &sh, r.body["job_id"].as_str().unwrap()).await;
    assert_eq!(job["state"], "failed", "{job}");
    assert!(job["error"].as_str().unwrap().contains("your limit is 1 s"), "{job}");
}

#[tokio::test]
async fn only_the_costly_routes_count_against_requests_per_minute() {
    let t = TestApp::new().await;
    let admin = t.setup_admin().await;
    let r = t
        .post(
            "/api/admin/roles",
            &admin,
            json!({ "name": "slow", "permissions": ["generate.ytpmv", "export.render"],
                    "quota": { "max_concurrent_jobs": 2, "max_render_seconds": 60, "max_height": 480, "requests_per_minute": 2 } }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let u = t.create_user(&admin, "sam", &["slow"]).await;

    // Uploading, renaming and deleting are free, however often.
    let mut id = String::new();
    for n in 0..5 {
        let up = upload(&t, &u, "a.mid", &song_midi()).await;
        assert_eq!(up.status, StatusCode::OK, "upload {n}: {:?}", up.body);
        id = up.body["midi_id"].as_str().unwrap().to_string();
    }
    for n in 0..5 {
        let r = t.call(Method::PATCH, &format!("/api/ytpmv/midi/{id}"), &u, Some(json!({ "title": format!("t{n}") }))).await;
        assert_eq!(r.status, StatusCode::OK, "rename {n}: {:?}", r.body);
    }

    // Making sound is not: two a minute, and then a Retry-After.
    let mix = json!({ "midi_id": id, "options": { "synth": true, "chip": "nes" }, "seconds": 1 });
    for n in 0..2 {
        let r = post_raw(&t, &u, "/api/ytpmv/preview-mix", mix.clone()).await;
        assert_eq!(r.status, StatusCode::OK, "mix {n}");
    }
    let r = t.post("/api/ytpmv/preview-mix", &u, mix).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{:?}", r.body);
    assert!(r.headers.contains_key(header::RETRY_AFTER));

    // ...while the library keeps working.
    assert_eq!(t.get("/api/ytpmv/library", &u).await.status, StatusCode::OK);
    let r = t.call(Method::DELETE, &format!("/api/ytpmv/midi/{id}"), &u, None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{:?}", r.body);
}