mod common;

use std::net::SocketAddr;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use common::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use ytp_core::{CorpusEvent, Event, JobState};

#[tokio::test]
async fn foreign_origin_is_refused() {
    let t = TestApp::new().await;
    let admin = t.setup_admin().await;
    let req = with_auth(
        Request::builder()
            .uri("/api/ws")
            .header(header::HOST, "localhost:8765")
            .header(header::ORIGIN, "http://evil.example"),
        &admin,
    )
    .body(Body::empty())
    .unwrap();
    let r = t.send(req).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn same_host_and_configured_origins_pass() {
    let t = TestApp::with_config(|c| {
        c.allowed_origins = vec!["http://localhost:5173".into()];
    })
    .await;
    let admin = t.setup_admin().await;
    for (origin, ok) in [
        ("http://localhost:8765", true),
        ("http://localhost:5173", true),
        ("http://localhost:5174", false),
        ("http://evil.example", false),
    ] {
        let req = with_auth(
            Request::builder()
                .uri("/api/ws")
                .header(header::HOST, "localhost:8765")
                .header(header::ORIGIN, origin),
            &admin,
        )
        .body(Body::empty())
        .unwrap();
        let r = t.send(req).await;
        // Past the origin check, oneshot cannot upgrade: that is a 400, not 403.
        assert_eq!(
            r.status == StatusCode::FORBIDDEN,
            !ok,
            "{origin}: {}",
            r.status
        );
    }
}

async fn next_json<S>(ws: &mut S) -> Value
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timed out")
            .expect("closed")
            .expect("ws error");
        if let Message::Text(t) = m {
            return serde_json::from_str(t.as_str()).unwrap();
        }
    }
}

#[tokio::test]
async fn hello_ping_and_events() {
    let t = TestApp::new().await;
    let admin = t.setup_admin().await;
    let Auth::Session { cookie, .. } = &admin else {
        unreachable!()
    };

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = t.app.clone();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });

    let mut req = format!("ws://{addr}/api/ws").into_client_request().unwrap();
    req.headers_mut()
        .insert("cookie", format!("ytp_session={cookie}").parse().unwrap());
    req.headers_mut()
        .insert("origin", format!("http://{addr}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    let hello = next_json(&mut ws).await;
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["user"]["username"], "admin");
    assert!(hello["permissions"].as_array().unwrap().len() > 10);

    ws.send(Message::Text(r#"{"type":"ping"}"#.into()))
        .await
        .unwrap();
    assert_eq!(next_json(&mut ws).await["type"], "pong");

    ws.send(Message::Text(
        r#"{"type":"subscribe","topic":"job:42"}"#.into(),
    ))
    .await
    .unwrap();
    // Round-trip so the subscription is in place before publishing.
    ws.send(Message::Text(r#"{"type":"ping"}"#.into()))
        .await
        .unwrap();
    assert_eq!(next_json(&mut ws).await["type"], "pong");

    t.st.publish(Event::Job {
        id: "7".into(),
        owner: 1,
        state: JobState::Running,
        progress: 0.1,
        message: None,
    });
    t.st.publish(Event::Job {
        id: "42".into(),
        owner: 1,
        state: JobState::Running,
        progress: 0.5,
        message: Some("half".into()),
    });
    let job = next_json(&mut ws).await;
    assert_eq!(job["type"], "job");
    assert_eq!(job["id"], "42", "unsubscribed job 7 must not arrive");
    assert_eq!(job["state"], "running");
    assert_eq!(job["message"], "half");

    t.st.publish(Event::Corpus {
        event: CorpusEvent::Installed,
        name: "x".into(),
    });
    let c = next_json(&mut ws).await;
    assert_eq!(c["type"], "corpus");
    assert_eq!(c["event"], "installed");
    assert_eq!(c["name"], "x");

    ws.send(Message::Text("nonsense".into())).await.unwrap();
    assert_eq!(next_json(&mut ws).await["type"], "error");

    // Without credentials the handshake fails with 401.
    let anon = tokio_tungstenite::connect_async(format!("ws://{addr}/api/ws")).await;
    match anon {
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED)
        }
        other => panic!("expected 401, got {other:?}"),
    }
}

#[tokio::test]
async fn query_token_auth() {
    let t = TestApp::new().await;
    let admin = t.setup_admin().await;
    let r = t
        .post(
            "/api/tokens",
            &admin,
            serde_json::json!({ "name": "ws", "permissions": ["corpus.read"] }),
        )
        .await;
    let secret = r.body["secret"].as_str().unwrap().to_string();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = t.app.clone();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/api/ws?token={secret}"))
            .await
            .unwrap();
    let hello = next_json(&mut ws).await;
    assert_eq!(hello["permissions"], serde_json::json!(["corpus.read"]));
}
