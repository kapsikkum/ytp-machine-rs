mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use common::*;
use serde_json::{json, Value};
use ytp_core::{Event, JobState};
use ytp_media::{RenderError, RenderJob, Renderer};
use ytp_splice::{AudioProbe, NoAudio};

/// Writes a few bytes instead of video, slowly enough to cancel.
struct Fake {
    delay: Duration,
    running: AtomicUsize,
    peak: AtomicUsize,
    fail: bool,
}

impl Fake {
    fn new(ms: u64) -> Arc<Self> {
        Arc::new(Self {
            delay: Duration::from_millis(ms),
            running: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            fail: false,
        })
    }
}

impl Renderer for Fake {
    fn render(
        &self,
        job: &RenderJob,
        progress: &dyn Fn(&str, usize, usize) -> bool,
    ) -> Result<(), RenderError> {
        let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        let res = (|| {
            for i in 0..10 {
                if !progress("encoding", i, 10) {
                    return Err(RenderError::Cancelled);
                }
                std::thread::sleep(self.delay / 10);
            }
            if self.fail {
                return Err(RenderError::Failed("ffmpeg failed:\nboom".into()));
            }
            std::fs::write(job.out, b"fake mp4 bytes").map_err(|e| RenderError::Failed(e.to_string()))
        })();
        self.running.fetch_sub(1, Ordering::SeqCst);
        res
    }

    fn glitches(&self) -> Vec<String> {
        vec!["shake".into()]
    }

    fn audio(&self) -> &dyn AudioProbe {
        &NoAudio
    }

    fn check(&self) -> Result<String, String> {
        Ok("fake".into())
    }
}

async fn setup(renderer: Arc<Fake>, workers: usize) -> (TestApp, Auth) {
    let t = TestApp::with_renderer(renderer, move |c| c.job_workers = workers).await;
    let admin = t.setup_admin().await;
    let bundle = make_bundle(t.dir.path(), &[]).await;
    let r = t
        .install(&admin, &[("file", Some("fixture.tar.zst"), &bundle)])
        .await;
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

async fn generate(t: &TestApp, auth: &Auth, body: Value) -> String {
    let r = t.post("/api/ytp/generate", auth, body).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    r.body["job_id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn generate_runs_a_job_to_a_result() {
    let (t, admin) = setup(Fake::new(50), 2).await;
    let mut events = t.st.events.subscribe();
    let id = generate(
        &t,
        &admin,
        json!({ "text": "hello world", "voice": "fixture", "captions": false, "options": { "seed": "abc", "speed": 1.5 } }),
    )
    .await;
    assert_eq!(id.len(), 20);

    let job = wait_for(&t, &admin, &id, &["done", "failed"]).await;
    assert_eq!(job["state"], "done", "{job}");
    assert_eq!(job["progress"], 1.0);
    assert_eq!(job["owner_name"], "admin");
    assert_eq!(job["voice"], "fixture");
    assert_eq!(job["options"]["seed"], "abc");
    assert_eq!(job["options"]["speed"], 1.5);
    assert_eq!(job["report"]["missing"], json!([]));
    assert_eq!(job["report"]["tokens"].as_array().unwrap().len(), 2);
    assert_eq!(job["timeline"].as_array().unwrap().len(), 2);
    assert!(job["duration"].as_f64().unwrap() > 0.3);
    assert_eq!(job["size_bytes"], 14);
    assert_eq!(job["result_url"], format!("/api/jobs/{id}/result"));
    assert!(job["finished_at"].as_str().unwrap().ends_with('Z'));

    // The result is served, with ranges.
    let r = t.get(&format!("/api/jobs/{id}/result"), &admin).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers[header::CONTENT_TYPE], "video/mp4");
    assert_eq!(r.body, Value::String("fake mp4 bytes".into()));
    let req = with_auth(
        Request::builder()
            .method(Method::GET)
            .uri(format!("/api/jobs/{id}/result"))
            .header(header::RANGE, "bytes=5-7"),
        &admin,
    )
    .body(Body::empty())
    .unwrap();
    let r = t.send(req).await;
    assert_eq!(r.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.body, Value::String("mp4".into()));

    // The bus saw queued -> running -> ... -> done, only for this job.
    let mut states = Vec::new();
    while let Ok(ev) = events.try_recv() {
        if let Event::Job { id: eid, state, owner, .. } = ev {
            assert_eq!(eid, id);
            assert_eq!(owner, 1);
            if states.last() != Some(&state) {
                states.push(state);
            }
        }
    }
    assert_eq!(states.first(), Some(&JobState::Queued));
    assert_eq!(states.get(1), Some(&JobState::Running));
    assert_eq!(states.last(), Some(&JobState::Done));
}

#[tokio::test]
async fn the_same_seed_makes_the_same_plan() {
    let (t, admin) = setup(Fake::new(10), 2).await;
    let body = json!({ "text": "hello world hello", "voice": "fixture", "options": { "seed": "same", "phrases": false } });
    let a = generate(&t, &admin, body.clone()).await;
    let b = generate(&t, &admin, body).await;
    let a = wait_for(&t, &admin, &a, &["done"]).await;
    let b = wait_for(&t, &admin, &b, &["done"]).await;
    assert_eq!(a["report"], b["report"]);
    assert_eq!(a["timeline"], b["timeline"]);
}

#[tokio::test]
async fn bad_requests_are_refused() {
    let (t, admin) = setup(Fake::new(10), 1).await;
    for (body, what) in [
        (json!({ "text": "   ", "voice": "fixture" }), "empty"),
        (json!({ "text": "hi", "voice": "nope" }), "unknown voice"),
        (json!({ "text": "hi", "voice": "fixture", "options": 5 }), "bad options"),
        (json!({ "text": "x".repeat(5000), "voice": "fixture" }), "too long"),
        (json!({ "voice": "fixture" }), "no text"),
    ] {
        let r = t.post("/api/ytp/generate", &admin, body).await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{what}: {:?}", r.body);
        assert_eq!(r.code(), "invalid", "{what}");
    }
    // Without a voice the first installed corpus is used.
    let id = generate(&t, &admin, json!({ "text": "hello" })).await;
    assert_eq!(wait_for(&t, &admin, &id, &["done"]).await["voice"], "fixture");
}

#[tokio::test]
async fn nothing_to_say_fails_with_the_missing_words() {
    let (t, admin) = setup(Fake::new(10), 1).await;
    let id = generate(&t, &admin, json!({ "text": "zzzz qqqq", "voice": "fixture" })).await;
    let job = wait_for(&t, &admin, &id, &["failed", "done"]).await;
    assert_eq!(job["state"], "failed", "{job}");
    assert!(job["error"].as_str().unwrap().contains("none of the words could be said"));
    assert_eq!(job["report"]["missing"], json!(["z", "q"]));
    assert!(job.get("result_url").is_none());
}

#[tokio::test]
async fn a_renderer_failure_fails_the_job_and_leaves_no_file() {
    let fake = Arc::new(Fake {
        delay: Duration::from_millis(10),
        running: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
        fail: true,
    });
    let (t, admin) = setup(fake, 1).await;
    let id = generate(&t, &admin, json!({ "text": "hello" })).await;
    let job = wait_for(&t, &admin, &id, &["failed", "done"]).await;
    assert_eq!(job["state"], "failed");
    assert!(job["error"].as_str().unwrap().contains("boom"));
    let r = t.get(&format!("/api/jobs/{id}/result"), &admin).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
}

async fn limited_user(t: &TestApp, admin: &Auth, name: &str, role: &str, quota: Value) -> Auth {
    let r = t
        .post(
            "/api/admin/roles",
            admin,
            json!({ "name": role, "permissions": ["generate.ytp", "corpus.read"], "quota": quota }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    t.create_user(admin, name, &[role]).await
}

fn quota(jobs: u32, secs: u32) -> Value {
    json!({ "max_concurrent_jobs": jobs, "max_render_seconds": secs, "max_height": 480, "requests_per_minute": 600 })
}

#[tokio::test]
async fn per_user_concurrency_follows_the_role_quota() {
    let fake = Fake::new(300);
    let (t, admin) = setup(fake.clone(), 4).await;
    let one = limited_user(&t, &admin, "solo", "solo", quota(1, 600)).await;
    let mut ids = Vec::new();
    for _ in 0..3 {
        ids.push(generate(&t, &one, json!({ "text": "hello", "voice": "fixture" })).await);
    }
    for id in &ids {
        wait_for(&t, &one, id, &["done"]).await;
    }
    assert_eq!(fake.peak.load(Ordering::SeqCst), 1, "one user, quota 1, four workers");

    // Two users may overlap.
    fake.peak.store(0, Ordering::SeqCst);
    let two = limited_user(&t, &admin, "duo", "duo", quota(1, 600)).await;
    let a = generate(&t, &one, json!({ "text": "hello", "voice": "fixture" })).await;
    let b = generate(&t, &two, json!({ "text": "hello", "voice": "fixture" })).await;
    wait_for(&t, &one, &a, &["done"]).await;
    wait_for(&t, &two, &b, &["done"]).await;
    assert_eq!(fake.peak.load(Ordering::SeqCst), 2);

    // A role allowed several at once gets several.
    fake.peak.store(0, Ordering::SeqCst);
    let many = limited_user(&t, &admin, "many", "many", quota(3, 600)).await;
    let ids: Vec<String> = {
        let mut v = Vec::new();
        for _ in 0..3 {
            v.push(generate(&t, &many, json!({ "text": "hello", "voice": "fixture" })).await);
        }
        v
    };
    for id in &ids {
        wait_for(&t, &many, id, &["done"]).await;
    }
    assert_eq!(fake.peak.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn too_many_queued_jobs_and_zero_quotas_are_refused() {
    let (t, admin) = setup(Fake::new(2000), 1).await;
    let u = limited_user(&t, &admin, "q", "q", quota(1, 600)).await;
    let mut last = StatusCode::OK;
    for _ in 0..6 {
        last = t
            .post("/api/ytp/generate", &u, json!({ "text": "hello", "voice": "fixture" }))
            .await
            .status;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);

    let none = limited_user(&t, &admin, "z", "z", quota(0, 600)).await;
    let r = t
        .post("/api/ytp/generate", &none, json!({ "text": "hello", "voice": "fixture" }))
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn max_render_seconds_limits_the_video() {
    let (t, admin) = setup(Fake::new(10), 1).await;
    let short = limited_user(&t, &admin, "short", "short", quota(2, 1)).await;
    let id = generate(
        &t,
        &short,
        json!({ "text": "hello world hello world hello world", "voice": "fixture", "options": { "phrases": false } }),
    )
    .await;
    let job = wait_for(&t, &short, &id, &["failed", "done"]).await;
    assert_eq!(job["state"], "failed", "{job}");
    let e = job["error"].as_str().unwrap();
    assert!(e.contains("your limit is 1 s"), "{e}");
    // Nothing rendered, but the plan is still reported.
    assert!(job["report"]["found"].as_array().unwrap().len() >= 6);
}

#[tokio::test]
async fn jobs_belong_to_their_owner() {
    let (t, admin) = setup(Fake::new(10), 2).await;
    let alice = t.create_user(&admin, "alice", &["creator"]).await;
    let bob = t.create_user(&admin, "bob", &["creator"]).await;
    let editor = t.create_user(&admin, "ed", &["editor"]).await;
    let id = generate(&t, &alice, json!({ "text": "hello", "voice": "fixture" })).await;
    wait_for(&t, &alice, &id, &["done"]).await;

    // Bob cannot tell it exists; admin and editor (jobs.view_all) can.
    for path in [format!("/api/jobs/{id}"), format!("/api/jobs/{id}/result")] {
        assert_eq!(t.get(&path, &bob).await.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(t.get(&path, &editor).await.status, StatusCode::OK, "{path}");
        assert_eq!(t.get(&path, &admin).await.status, StatusCode::OK, "{path}");
    }
    let url = format!("/api/jobs/{id}");
    assert_eq!(t.call(Method::DELETE, &url, &bob, None).await.status, StatusCode::NOT_FOUND);
    // viewing is not cancelling
    let r = t.call(Method::DELETE, &url, &editor, None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{:?}", r.body);
    assert_eq!(t.get(&format!("/api/jobs/{id}"), &alice).await.status, StatusCode::OK);
    // jobs.cancel_any can, and it is audited
    assert_eq!(t.call(Method::DELETE, &url, &admin, None).await.status, StatusCode::NO_CONTENT);
    assert_eq!(t.get(&format!("/api/jobs/{id}"), &alice).await.status, StatusCode::NOT_FOUND);
    let audit = t.get("/api/admin/audit?limit=20", &admin).await;
    assert!(audit.body.as_array().unwrap().iter().any(|e| e["action"] == "job.cancel" && e["target"] == id.as_str()));
}

#[tokio::test]
async fn cancelling_queued_and_running_jobs() {
    let fake = Fake::new(1500);
    let (t, admin) = setup(fake.clone(), 1).await;
    let u = t.create_user(&admin, "carol", &["creator"]).await;
    let running = generate(&t, &u, json!({ "text": "hello", "voice": "fixture" })).await;
    let queued = generate(&t, &u, json!({ "text": "hello", "voice": "fixture" })).await;
    wait_for(&t, &u, &running, &["running"]).await;
    assert_eq!(t.get(&format!("/api/jobs/{queued}"), &u).await.body["state"], "queued");

    let r = t.call(Method::DELETE, &format!("/api/jobs/{queued}"), &u, None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(t.get(&format!("/api/jobs/{queued}"), &u).await.body["state"], "cancelled");

    let r = t.call(Method::DELETE, &format!("/api/jobs/{running}"), &u, None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let job = wait_for(&t, &u, &running, &["cancelled", "done"]).await;
    assert_eq!(job["state"], "cancelled");
    assert_eq!(t.get(&format!("/api/jobs/{running}/result"), &u).await.status, StatusCode::CONFLICT);
    // the cancelled job never ran the queued one either
    assert_eq!(t.get(&format!("/api/jobs/{queued}"), &u).await.body["state"], "cancelled");
    assert_eq!(fake.running.load(Ordering::SeqCst), 0);

    // Deleting a finished job removes it and its files.
    let done = generate(&t, &u, json!({ "text": "hello", "voice": "fixture" })).await;
    t.call(Method::DELETE, &format!("/api/jobs/{running}"), &u, None).await;
    wait_for(&t, &u, &done, &["done"]).await;
    let out = t.dir.path().join("data").join("output");
    assert!(out.join(format!("{done}.mp4")).is_file());
    let r = t.call(Method::DELETE, &format!("/api/jobs/{done}"), &u, None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert!(!out.join(format!("{done}.mp4")).exists());
    assert!(!out.join(format!("{done}.json")).exists());
    assert_eq!(t.get(&format!("/api/jobs/{done}"), &u).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn finished_jobs_survive_a_restart() {
    let (t, admin) = setup(Fake::new(10), 1).await;
    let id = generate(&t, &admin, json!({ "text": "hello world", "voice": "fixture" })).await;
    let job = wait_for(&t, &admin, &id, &["done"]).await;
    // A second server over the same data directory knows the job.
    let config = ytp_server::Config::new(t.dir.path().join("data"));
    let st2 = ytp_server::AppState::with_renderer(config, Fake::new(10)).await.unwrap();
    let again = st2.jobs.get(&id).expect("job reloaded");
    assert_eq!(again.state, JobState::Done);
    assert_eq!(serde_json::to_value(&again).unwrap()["timeline"], job["timeline"]);
    assert!(st2.jobs.result_path(&id).is_file());
}

#[tokio::test]
async fn the_guest_role_can_generate_and_tokens_need_a_job_permission() {
    let (t, admin) = setup(Fake::new(10), 1).await;
    let guest = t.create_user(&admin, "gg", &["guest"]).await;
    let id = generate(&t, &guest, json!({ "text": "hello", "voice": "fixture" })).await;
    wait_for(&t, &guest, &id, &["done"]).await;

    let r = t
        .post("/api/tokens", &admin, json!({ "name": "narrow", "permissions": ["corpus.read"] }))
        .await;
    let narrow = Auth::Bearer(r.body["secret"].as_str().unwrap().to_string());
    // admin's token without any job permission cannot even look
    assert_eq!(t.get(&format!("/api/jobs/{id}"), &narrow).await.status, StatusCode::FORBIDDEN);
    let r = t
        .post("/api/tokens", &admin, json!({ "name": "gen", "permissions": ["generate.ytp"] }))
        .await;
    let gen = Auth::Bearer(r.body["secret"].as_str().unwrap().to_string());
    let id2 = generate(&t, &gen, json!({ "text": "hello", "voice": "fixture" })).await;
    wait_for(&t, &gen, &id2, &["done"]).await;
}

#[tokio::test]
async fn job_events_reach_websocket_subscribers() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message;

    let (t, admin) = setup(Fake::new(200), 1).await;
    let Auth::Session { cookie, csrf } = &admin else { unreachable!() };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = t.app.clone();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let mut req = format!("ws://{addr}/api/ws").into_client_request().unwrap();
    req.headers_mut().insert("cookie", format!("ytp_session={cookie}").parse().unwrap());
    req.headers_mut().insert("origin", format!("http://{addr}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    ws.send(Message::Text(r#"{"type":"subscribe","topic":"jobs"}"#.into())).await.unwrap();

    let http = format!("http://{addr}");
    let _ = (&http, csrf);
    let id = generate(&t, &admin, json!({ "text": "hello", "voice": "fixture" })).await;

    let mut seen: Vec<(String, String)> = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        let Ok(Some(Ok(Message::Text(m)))) =
            tokio::time::timeout(Duration::from_secs(3), ws.next()).await
        else {
            continue;
        };
        let v: Value = serde_json::from_str(m.as_str()).unwrap();
        if v["type"] == "job" {
            assert_eq!(v["id"], id.as_str());
            seen.push((v["state"].as_str().unwrap().into(), v["message"].as_str().unwrap_or("").into()));
            if v["state"] == "done" {
                break;
            }
        }
    }
    let states: Vec<&str> = seen.iter().map(|s| s.0.as_str()).collect();
    assert_eq!(states.first(), Some(&"queued"), "{seen:?}");
    assert!(states.contains(&"running"));
    assert_eq!(states.last(), Some(&"done"));
    assert!(seen.iter().any(|(_, m)| m.starts_with("encoding")), "{seen:?}");
}
