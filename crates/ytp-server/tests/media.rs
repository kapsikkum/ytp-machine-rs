//! `/api/media`: uploads are private to the user who made them, the renderer
//! only ever looks inside the project owner's directory, and the listing
//! reports how many stored projects mention an asset. Skipped when ffmpeg is
//! not on PATH (or YTP_FFMPEG), like the render tests.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use common::{multipart, with_auth, Auth, TestApp};
use serde_json::{json, Value};

fn ffmpeg() -> PathBuf {
    std::env::var("YTP_FFMPEG").map(PathBuf::from).unwrap_or_else(|_| "ffmpeg".into())
}

fn have_ffmpeg() -> bool {
    Command::new(ffmpeg())
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A tiny real mp4 (0.4 s, 64x36, h264 + aac) for ffprobe to read.
fn make_mp4(dir: &Path) -> Vec<u8> {
    let path = dir.join("clip.mp4");
    let ok = Command::new(ffmpeg())
        .args(["-y", "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg("color=c=red:s=64x36:r=25:d=0.4")
        .args(["-f", "lavfi", "-i", "anullsrc=r=44100:cl=stereo"])
        .args(["-t", "0.4", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac"])
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success());
    assert!(ok, "ffmpeg could not make the fixture clip");
    std::fs::read(&path).unwrap()
}

/// Upload one file and answer the parsed response.
async fn upload(t: &TestApp, auth: &Auth, filename: &str, bytes: &[u8]) -> common::Resp {
    let (ct, body) = multipart(&[("file", Some(filename), bytes)]);
    let req = with_auth(
        Request::builder()
            .method(Method::POST)
            .uri("/api/media")
            .header(header::CONTENT_TYPE, ct),
        auth,
    )
    .body(Body::from(body))
    .unwrap();
    t.send(req).await
}

/// One video track holding a single `media` clip that names `asset`.
fn media_project(name: &str, asset: &str) -> Value {
    json!({
        "version": 1,
        "name": name,
        "corpus": "",
        "fps": 25,
        "width": 480,
        "height": 270,
        "captions": false,
        "tracks": [{
            "id": "v1",
            "kind": "video",
            "name": "Video 1",
            "mute": false,
            "solo": false,
            "volume": 1.0,
            "generated": false,
            "clips": [{
                "id": "m1",
                "start": 0.0,
                "duration": 0.4,
                "source": { "type": "media", "asset": asset, "in": 0.0, "out": 0.4 },
                "effects": []
            }]
        }]
    })
}

#[tokio::test]
async fn an_upload_is_stored_with_its_ffprobe_reading_and_counts_references() {
    if !have_ffmpeg() {
        return;
    }
    let t = TestApp::new().await;
    let admin = t.setup_admin().await;
    let me = t.get("/api/auth/state", &admin).await;
    let owner = me.body["user"]["id"].as_i64().unwrap();

    let bytes = make_mp4(t.dir.path());
    let r = upload(&t, &admin, "clip.mp4", &bytes).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let id = r.body["id"].as_str().unwrap().to_string();
    assert_eq!(id.len(), 12, "an asset id looks like a MIDI file's: {id}");
    assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(r.body["ext"], "mp4");
    assert_eq!(r.body["kind"], "video");
    assert_eq!(r.body["name"], "clip");
    assert_eq!(r.body["bytes"], bytes.len());
    assert_eq!(r.body["referenced_by"], 0);
    // ffprobe's reading, stored rather than guessed.
    assert_eq!(r.body["width"], 64);
    assert_eq!(r.body["height"], 36);
    let duration = r.body["duration"].as_f64().expect("a duration");
    assert!((duration - 0.4).abs() < 0.1, "duration {duration}");
    assert!(r.body["video_codec"].as_str().is_some_and(|c| c.contains("264")), "{:?}", r.body["video_codec"]);
    assert_eq!(r.body["audio_codec"], "aac");

    // The file and its sidecar are where the docs say they are.
    let dir = t.st.config.data_dir.join("media").join(owner.to_string());
    let file = dir.join(format!("{id}.mp4"));
    let sidecar = dir.join(format!("{id}.json"));
    assert!(file.is_file(), "{}", file.display());
    assert!(sidecar.is_file(), "{}", sidecar.display());
    let stored: Value = serde_json::from_slice(&std::fs::read(&sidecar).unwrap()).unwrap();
    assert_eq!(stored["id"], id);

    let list = t.get("/api/media", &admin).await;
    assert_eq!(list.status, StatusCode::OK);
    assert_eq!(list.body.as_array().unwrap().len(), 1);

    // A stored project that names the asset moves the count.
    let created = t
        .post("/api/projects", &admin, json!({ "project": media_project("media", &id) }))
        .await;
    assert_eq!(created.status, StatusCode::OK, "{:?}", created.body);
    let list = t.get("/api/media", &admin).await;
    assert_eq!(list.body[0]["referenced_by"], 1);

    // Deleting removes both files and says how many projects now dangle.
    let del = t
        .call(Method::DELETE, &format!("/api/media/{id}"), &admin, None)
        .await;
    assert_eq!(del.status, StatusCode::NO_CONTENT, "{:?}", del.body);
    assert_eq!(del.headers.get("x-media-referenced-by").unwrap(), "1");
    assert!(!file.exists() && !sidecar.exists());
    assert_eq!(t.get("/api/media", &admin).await.body, json!([]));
    assert_eq!(
        t.call(Method::DELETE, "/api/media/deadbeef1234", &admin, None).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn another_users_asset_is_invisible_and_unplayable() {
    if !have_ffmpeg() {
        return;
    }
    let t = TestApp::new().await;
    let admin = t.setup_admin().await;
    let bytes = make_mp4(t.dir.path());
    let r = upload(&t, &admin, "mine.mp4", &bytes).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let id = r.body["id"].as_str().unwrap().to_string();

    // An editor holds media.install, so this is isolation, not a permission miss.
    let bob = t.create_user(&admin, "bob", &["editor"]).await;
    assert_eq!(t.get("/api/media", &bob).await.body, json!([]));
    assert_eq!(
        t.get("/api/media?all=1", &bob).await.status,
        StatusCode::FORBIDDEN,
        "only an admin lists everyone's media"
    );
    assert_eq!(
        t.call(Method::DELETE, &format!("/api/media/{id}"), &bob, None).await.status,
        StatusCode::NOT_FOUND,
        "somebody else's asset does not exist for bob"
    );

    // Naming admin's asset in bob's own project must be refused at render time.
    let created = t
        .post("/api/projects", &bob, json!({ "project": media_project("borrowed", &id) }))
        .await;
    assert_eq!(created.status, StatusCode::OK, "{:?}", created.body);
    let pid = created.body["id"].as_i64().unwrap();
    let preview = t
        .post("/api/preview", &bob, json!({ "project_id": pid, "start": 0.0, "duration": 0.4 }))
        .await;
    assert_eq!(preview.status, StatusCode::UNPROCESSABLE_ENTITY, "{:?}", preview.body);
    let message = preview.body["message"].as_str().unwrap_or("");
    assert!(
        message.contains("is not in this project owner's media library"),
        "{message}"
    );

    // The owner can still see and use it.
    assert_eq!(t.get("/api/media", &admin).await.body.as_array().unwrap().len(), 1);
    let mine = t
        .post("/api/projects", &admin, json!({ "project": media_project("mine", &id) }))
        .await;
    assert_eq!(mine.status, StatusCode::OK, "{:?}", mine.body);
    let my_pid = mine.body["id"].as_i64().unwrap();
    let preview = t
        .post("/api/preview", &admin, json!({ "project_id": my_pid, "start": 0.0, "duration": 0.4 }))
        .await;
    assert_eq!(preview.status, StatusCode::OK, "{:?}", preview.body);
    assert_eq!(
        preview.headers.get(header::CONTENT_TYPE).unwrap(),
        "video/mp4",
        "the owner's media clip renders"
    );
}

#[tokio::test]
async fn only_the_accepted_types_and_real_streams_get_in() {
    if !have_ffmpeg() {
        return;
    }
    let t = TestApp::new().await;
    let admin = t.setup_admin().await;

    let notes = upload(&t, &admin, "notes.txt", b"just words").await;
    assert_eq!(notes.status, StatusCode::UNPROCESSABLE_ENTITY, "{:?}", notes.body);
    assert!(
        notes.body["message"].as_str().unwrap_or("").contains("not accepted"),
        "{:?}",
        notes.body
    );

    // Accepted extension, but no stream ffprobe can decode.
    let fake = upload(&t, &admin, "fake.mp4", b"not really a video").await;
    assert_eq!(fake.status, StatusCode::UNPROCESSABLE_ENTITY, "{:?}", fake.body);
    assert!(
        fake.body["message"].as_str().unwrap_or("").contains("no audio or video stream"),
        "{:?}",
        fake.body
    );

    let empty = upload(&t, &admin, "empty.mp4", b"").await;
    assert_eq!(empty.status, StatusCode::UNPROCESSABLE_ENTITY, "{:?}", empty.body);

    // A creator does not hold media.install at all.
    let creator = t.create_user(&admin, "cara", &["creator"]).await;
    let denied = upload(&t, &creator, "clip.mp4", b"whatever").await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN, "{:?}", denied.body);
    assert_eq!(t.get("/api/media", &creator).await.status, StatusCode::FORBIDDEN);

    // Nothing half-written survives a refusal.
    let dir = t.st.config.data_dir.join("media");
    let leftovers = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("upload-"))
                .count()
        })
        .unwrap_or(0);
    assert_eq!(leftovers, 0, "a refused upload left a spool file behind");
}
