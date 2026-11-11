#![allow(dead_code)]

use std::path::Path;

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
use sqlx::{ConnectOptions, Connection};
use tempfile::TempDir;
use tower::ServiceExt;
use ytp_server::{app, AppState, Config};

pub struct TestApp {
    pub app: Router,
    pub st: AppState,
    pub dir: TempDir,
}

#[derive(Clone, Debug, Default)]
pub enum Auth {
    #[default]
    None,
    Session {
        cookie: String,
        csrf: String,
    },
    /// A session cookie but no CSRF header.
    CookieOnly(String),
    Bearer(String),
}

pub struct Resp {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Value,
}

impl Resp {
    pub fn code(&self) -> &str {
        self.body["error"].as_str().unwrap_or("")
    }
}

pub const ADMIN_PW: &str = "admin-password-1";

impl TestApp {
    pub async fn new() -> Self {
        Self::with_config(|_| {}).await
    }

    pub async fn with_config(tweak: impl FnOnce(&mut Config)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::new(dir.path().join("data"));
        tweak(&mut config);
        let st = AppState::new(config).await.unwrap();
        Self {
            app: app(st.clone()),
            st,
            dir,
        }
    }

    pub async fn send(&self, req: Request<Body>) -> Resp {
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()))
        };
        Resp {
            status,
            headers,
            body,
        }
    }

    pub async fn call(&self, method: Method, uri: &str, auth: &Auth, body: Option<Value>) -> Resp {
        let mut b = Request::builder().method(method).uri(uri);
        b = with_auth(b, auth);
        let req = match body {
            Some(v) => b
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(v.to_string()))
                .unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
        self.send(req).await
    }

    pub async fn get(&self, uri: &str, auth: &Auth) -> Resp {
        self.call(Method::GET, uri, auth, None).await
    }

    pub async fn post(&self, uri: &str, auth: &Auth, body: Value) -> Resp {
        self.call(Method::POST, uri, auth, Some(body)).await
    }

    /// Complete first-run setup; returns the admin session.
    pub async fn setup_admin(&self) -> Auth {
        let token = self.st.setup_token().await.expect("setup token");
        let r = self
            .post(
                "/api/auth/setup",
                &Auth::None,
                json!({ "token": token, "username": "admin", "password": ADMIN_PW }),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
        let cookie = session_cookie(&r.headers).expect("cookie");
        let csrf = r.body["csrf"].as_str().expect("setup returns csrf");
        assert!(r.body["permissions"].is_array());
        let s = self
            .get("/api/auth/state", &Auth::CookieOnly(cookie.clone()))
            .await;
        assert_eq!(s.body["csrf"], csrf);
        Auth::Session {
            cookie,
            csrf: csrf.to_string(),
        }
    }

    pub async fn login(&self, username: &str, password: &str) -> Auth {
        let r = self
            .post(
                "/api/auth/login",
                &Auth::None,
                json!({ "username": username, "password": password }),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
        Auth::Session {
            cookie: session_cookie(&r.headers).expect("cookie"),
            csrf: r.body["csrf"].as_str().unwrap().to_string(),
        }
    }

    pub async fn create_user(&self, admin: &Auth, name: &str, roles: &[&str]) -> Auth {
        let r = self
            .post(
                "/api/admin/users",
                admin,
                json!({ "username": name, "password": "user-password-1", "roles": roles }),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
        self.login(name, "user-password-1").await
    }
}

pub fn with_auth(mut b: axum::http::request::Builder, auth: &Auth) -> axum::http::request::Builder {
    match auth {
        Auth::None => {}
        Auth::Session { cookie, csrf } => {
            b = b
                .header(header::COOKIE, format!("ytp_session={cookie}"))
                .header("x-csrf-token", csrf);
        }
        Auth::CookieOnly(cookie) => {
            b = b.header(header::COOKIE, format!("ytp_session={cookie}"));
        }
        Auth::Bearer(t) => {
            b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
    }
    b
}

pub fn session_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|v| v.strip_prefix("ytp_session="))
        .map(|v| v.split(';').next().unwrap().to_string())
        .filter(|v| !v.is_empty())
}

/// A minimal corpus database in the Python app's schema.
pub async fn make_corpus_db(path: &Path) {
    let mut conn = SqliteConnectOptions::new()
        .filename(path)
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
        "INSERT INTO sources (video_id, source_file) VALUES ('v1', 'downloads/v1.mp4'), ('v2', 'downloads/v2.mp4')",
        "INSERT INTO word_clips (source_id, word, start_time, end_time, source_file) VALUES
            (1, 'hello', 0.0, 0.4, 'downloads/v1.mp4'),
            (1, 'world', 0.5, 0.9, 'downloads/v1.mp4'),
            (2, 'hello', 1.0, 1.3, 'downloads/v2.mp4')",
    ] {
        sqlx::query(sql).execute(&mut conn).await.unwrap();
    }
    conn.close().await.unwrap();
}

/// One tar member with a raw (unvalidated) path and type.
pub fn raw_entry(
    b: &mut tar::Builder<Vec<u8>>,
    path: &str,
    kind: tar::EntryType,
    data: &[u8],
    link: Option<&str>,
) {
    let mut h = tar::Header::new_gnu();
    {
        let gnu = h.as_gnu_mut().unwrap();
        gnu.name[..path.len()].copy_from_slice(path.as_bytes());
        if let Some(l) = link {
            gnu.linkname[..l.len()].copy_from_slice(l.as_bytes());
        }
    }
    h.set_entry_type(kind);
    h.set_size(data.len() as u64);
    h.set_mode(0o644);
    h.set_cksum();
    b.append(&h, data).unwrap();
}

/// A valid bundle; `extra` adds further raw members.
pub async fn make_bundle(
    work: &Path,
    extra: &[(&str, tar::EntryType, &[u8], Option<&str>)],
) -> Vec<u8> {
    let db = work.join("fixture-corpus.db");
    let _ = std::fs::remove_file(&db);
    make_corpus_db(&db).await;
    let db_bytes = std::fs::read(&db).unwrap();
    let mut b = tar::Builder::new(Vec::new());
    raw_entry(
        &mut b,
        "corpus.db",
        tar::EntryType::Regular,
        &db_bytes,
        None,
    );
    raw_entry(&mut b, "downloads/", tar::EntryType::Directory, b"", None);
    raw_entry(
        &mut b,
        "downloads/v1.mp4",
        tar::EntryType::Regular,
        b"fake video",
        None,
    );
    for (p, k, d, l) in extra {
        raw_entry(&mut b, p, *k, d, *l);
    }
    let tar = b.into_inner().unwrap();
    zstd::encode_all(&tar[..], 3).unwrap()
}

pub fn multipart(fields: &[(&str, Option<&str>, &[u8])]) -> (String, Vec<u8>) {
    let boundary = "----ytptestboundary7MA4YWxkTrZu0gW";
    let mut body = Vec::new();
    for (name, filename, data) in fields {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        match filename {
            Some(f) => body.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"{name}\"; filename=\"{f}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
                )
                .as_bytes(),
            ),
            None => body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
            ),
        }
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

impl TestApp {
    pub async fn install(&self, auth: &Auth, fields: &[(&str, Option<&str>, &[u8])]) -> Resp {
        let (ct, body) = multipart(fields);
        let req = with_auth(
            Request::builder()
                .method(Method::POST)
                .uri("/api/corpora/install")
                .header(header::CONTENT_TYPE, ct),
            auth,
        )
        .body(Body::from(body))
        .unwrap();
        self.send(req).await
    }
}
