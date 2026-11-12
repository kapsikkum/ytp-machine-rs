//! The encode API: presets, exports from a kept master, and the hardware
//! report. The export tests need ffmpeg and are skipped without it.

mod common;

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use axum::http::{header, Method, StatusCode};
use common::*;
use serde_json::{json, Value};
use ytp_media::{RenderError, RenderJob, Renderer};
use ytp_splice::{AudioProbe, NoAudio};

/// Writes a placeholder result; tests that need a real encode overwrite the
/// master with one made by ffmpeg.
struct Fake;

impl Renderer for Fake {
    fn render(
        &self,
        job: &RenderJob,
        progress: &dyn Fn(&str, usize, usize) -> bool,
    ) -> Result<(), RenderError> {
        if !progress("encoding", 0, 1) {
            return Err(RenderError::Cancelled);
        }
        std::fs::write(job.out, b"fake mp4 bytes").map_err(|e| RenderError::Failed(e.to_string()))
    }

    fn glitches(&self) -> Vec<String> {
        Vec::new()
    }

    fn audio(&self) -> &dyn AudioProbe {
        &NoAudio
    }

    fn check(&self) -> Result<String, String> {
        Ok("fake".into())
    }
}

fn ffmpeg() -> PathBuf {
    std::env::var("YTP_FFMPEG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| "ffmpeg".into())
}

fn have_ffmpeg() -> bool {
    Command::new(ffmpeg())
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

async fn setup() -> (TestApp, Auth) {
    let t = TestApp::with_renderer(Arc::new(Fake), |c| c.job_workers = 2).await;
    let admin = t.setup_admin().await;
    let bundle = make_bundle(t.dir.path(), &[]).await;
    let r = t.install(&admin, &[("file", Some("fixture.tar.zst"), &bundle)]).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    (t, admin)
}

async fn wait_for(t: &TestApp, auth: &Auth, id: &str, want: &[&str]) -> Value {
    for _ in 0..200 {
        let r = t.get(&format!("/api/jobs/{id}"), auth).await;
        assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
        if want.contains(&r.body["state"].as_str().unwrap()) {
            return r.body;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("job {id} never reached {want:?}");
}

async fn generate(t: &TestApp, auth: &Auth) -> String {
    let r = t
        .post("/api/ytp/generate", auth, json!({ "text": "hello", "voice": "fixture" }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    r.body["job_id"].as_str().unwrap().to_string()
}

fn output_dir(t: &TestApp) -> PathBuf {
    t.dir.path().join("data").join("output")
}

/// A one-second 320x240 H.264 + AAC file where the renderer would have left
/// its master.
fn write_master(path: &PathBuf) {
    let ok = Command::new(ffmpeg())
        .args(["-y", "-v", "error", "-nostdin"])
        .args(["-f", "lavfi", "-i", "testsrc=size=320x240:rate=25"])
        .args(["-f", "lavfi", "-i", "sine=frequency=330:sample_rate=48000"])
        .args(["-t", "1", "-pix_fmt", "yuv420p", "-c:v", "libx264"])
        .args(["-preset", "ultrafast", "-c:a", "aac", "-shortest"])
        .arg(path)
        .status()
        .unwrap()
        .success();
    assert!(ok, "could not make the master");
}

#[tokio::test]
async fn presets_are_listed_and_hq_is_gated() {
    let (t, admin) = setup().await;
    let r = t.get("/api/encode/presets", &admin).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["hq"], true);
    let presets = r.body["presets"].as_array().unwrap();
    let find = |name: &str| presets.iter().find(|p| p["name"] == name).cloned();

    let web = find("web-default").expect("web-default");
    assert_eq!(web["hq"], false);
    assert_eq!(web["allowed"], true);
    assert_eq!(web["container"], "mp4");
    assert_eq!(web["video_codec"], "h264");

    let hq = find("hq-archive").expect("hq-archive");
    assert_eq!(hq["hq"], true);
    assert_eq!(hq["allowed"], true);
    assert!(find("matrix").is_some());
    assert!(find("gif").is_some());
    assert!(find("audio-only").is_some());

    // A creator may export, but not the archival presets.
    let creator = t.create_user(&admin, "cora", &["creator"]).await;
    let r = t.get("/api/encode/presets", &creator).await;
    assert_eq!(r.status, StatusCode::OK);
    let presets = r.body["presets"].as_array().unwrap();
    let hq = presets.iter().find(|p| p["name"] == "hq-archive").unwrap();
    assert_eq!(hq["hq"], true);
    assert_eq!(hq["allowed"], false);
    let web = presets.iter().find(|p| p["name"] == "web-default").unwrap();
    assert_eq!(web["allowed"], true);
}

#[tokio::test]
async fn the_encoder_report_lists_the_hardware_candidates() {
    let (t, admin) = setup().await;
    let r = t.get("/api/admin/encoders", &admin).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert!(r.body["ffmpeg"].is_string());
    assert_eq!(r.body["encoders"].as_array().unwrap().len(), 4);
    assert!(r.body["presets"].as_u64().unwrap() >= 8);
    for e in r.body["encoders"].as_array().unwrap() {
        assert!(e["encoder"].is_string());
        assert!(e["available"].is_boolean());
        assert!(e["detail"].is_string());
    }
}

#[tokio::test]
async fn export_needs_a_finished_source_with_a_video_file() {
    let (t, admin) = setup().await;
    let creator = t.create_user(&admin, "carl", &["creator"]).await;
    let id = generate(&t, &creator).await;
    wait_for(&t, &creator, &id, &["done"]).await;

    // An unknown preset is a bad request.
    let r = t
        .post(&format!("/api/jobs/{id}/export"), &creator, json!({ "preset": "nope" }))
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.code(), "invalid");

    // Someone else's job does not exist.
    let other = t.create_user(&admin, "olive", &["creator"]).await;
    let r = t
        .post(&format!("/api/jobs/{id}/export"), &other, json!({ "preset": "web-default" }))
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // The source result was never a real video and there is no master.
    std::fs::remove_file(output_dir(&t).join(format!("{id}.mp4"))).unwrap();
    let r = t
        .post(&format!("/api/jobs/{id}/export"), &creator, json!({ "preset": "web-default" }))
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{:?}", r.body);
}

#[tokio::test]
async fn export_reencodes_the_kept_master() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg is not available");
        return;
    }
    let (t, admin) = setup().await;
    let creator = t.create_user(&admin, "cory", &["creator"]).await;
    let id = generate(&t, &creator).await;
    wait_for(&t, &creator, &id, &["done"]).await;
    let out = output_dir(&t);
    write_master(&out.join(format!("{id}.master.mp4")));

    // hq presets need export.hq, which a creator does not have.
    let r = t
        .post(&format!("/api/jobs/{id}/export"), &creator, json!({ "preset": "hq-archive" }))
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    let r = t
        .post(&format!("/api/jobs/{id}/export"), &creator, json!({ "preset": "web-default" }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let export = r.body["job_id"].as_str().unwrap().to_string();
    let job = wait_for(&t, &creator, &export, &["done", "failed"]).await;
    assert_eq!(job["state"], "done", "{job}");
    assert_eq!(job["kind"], "encode");
    assert_eq!(job["report"]["source_job"], id.as_str());
    assert_eq!(job["report"]["preset"]["name"], "web-default");
    assert_eq!(job["report"]["width"], 320);
    assert_eq!(job["report"]["height"], 240);
    assert!(job["duration"].as_f64().unwrap() > 0.5);
    assert!(job["size_bytes"].as_u64().unwrap() > 1000);
    assert_eq!(job["result_url"], format!("/api/jobs/{export}/result"));

    let r = t.get(&format!("/api/jobs/{export}/result"), &creator).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers[header::CONTENT_TYPE], "video/mp4");
    assert!(r.headers[header::CONTENT_DISPOSITION]
        .to_str()
        .unwrap()
        .contains(&format!("ytp-{export}.mp4")));
    assert!(r.body.as_str().is_some_and(|s| !s.is_empty()));

    // The master is kept for the next export; the result is beside it.
    assert!(out.join(format!("{export}.mp4")).is_file());
    assert!(out.join(format!("{id}.master.mp4")).is_file());

    // A GIF export lands in `<id>.gif` and is served as one.
    let r = t
        .post(&format!("/api/jobs/{id}/export"), &creator, json!({ "preset": "gif" }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let gif = r.body["job_id"].as_str().unwrap().to_string();
    let job = wait_for(&t, &creator, &gif, &["done", "failed"]).await;
    assert_eq!(job["state"], "done", "{job}");
    let file = job["result_file"].as_str().unwrap_or_default();
    assert_eq!(file, format!("{gif}.gif"), "{job}");
    let r = t.get(&format!("/api/jobs/{gif}/result"), &creator).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers[header::CONTENT_TYPE], "image/gif");

    // Deleting the export removes its own file, not the source's master.
    let r = t
        .call(Method::DELETE, &format!("/api/jobs/{gif}"), &creator, None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert!(!out.join(format!("{gif}.gif")).exists());
    assert!(!out.join(format!("{gif}.json")).exists());
    assert!(out.join(format!("{id}.master.mp4")).is_file());
}
