mod common;

use axum::http::{Method, StatusCode};
use common::*;
use tar::EntryType;

fn corpora_dir(t: &TestApp) -> std::path::PathBuf {
    t.dir.path().join("data").join("corpora")
}

/// Everything in the corpora dir except leftovers that should not exist.
fn entries(t: &TestApp) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(corpora_dir(t))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn install_list_delete() {
    let t = TestApp::new().await;
    let admin = t.setup_admin().await;
    let bundle = make_bundle(t.dir.path(), &[]).await;

    let r = t
        .install(&admin, &[("file", Some("Test Corpus.tar.zst"), &bundle)])
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["name"], "test-corpus");
    assert_eq!(r.body["words"], 2);
    assert_eq!(r.body["clips"], 3);
    assert_eq!(r.body["sources"], 2);
    assert!(r.body["size_bytes"].as_u64().unwrap() > 0);
    assert!(r.body["installed_at"].as_str().unwrap().ends_with('Z'));
    assert!(corpora_dir(&t)
        .join("test-corpus")
        .join("downloads")
        .join("v1.mp4")
        .is_file());

    let list = t.get("/api/corpora", &admin).await;
    assert_eq!(list.body.as_array().unwrap().len(), 1);
    assert_eq!(list.body[0]["name"], "test-corpus");

    // Same name again: conflict, unless replacing.
    let r = t
        .install(
            &admin,
            &[
                ("file", Some("x.tar.zst"), &bundle),
                ("name", None, b"test-corpus"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let r = t
        .install(
            &admin,
            &[
                ("name", None, b"test-corpus"),
                ("replace", None, b"true"),
                ("file", Some("x.tar.zst"), &bundle),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);

    let words = t.st.corpora.words("test-corpus", 10).await.unwrap();
    assert_eq!(words[0], ("hello".to_string(), 2));

    let r = t
        .call(Method::DELETE, "/api/corpora/test-corpus", &admin, None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(
        t.get("/api/corpora", &admin).await.body,
        serde_json::json!([])
    );
    assert!(entries(&t).is_empty(), "{:?}", entries(&t));
    let r = t
        .call(Method::DELETE, "/api/corpora/test-corpus", &admin, None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = t
        .call(Method::DELETE, "/api/corpora/..", &admin, None)
        .await;
    assert_ne!(r.status, StatusCode::NO_CONTENT);
}

async fn assert_rejected(extra: &[(&str, EntryType, &[u8], Option<&str>)], why: &str) {
    let t = TestApp::new().await;
    let admin = t.setup_admin().await;
    let bundle = make_bundle(t.dir.path(), extra).await;
    let r = t
        .install(&admin, &[("file", Some("evil.tar.zst"), &bundle)])
        .await;
    assert_eq!(
        r.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{why}: {:?}",
        r.body
    );
    assert!(entries(&t).is_empty(), "{why}: left {:?}", entries(&t));
    assert!(
        !t.dir.path().join("data").join("evil.txt").exists()
            && !t.dir.path().join("evil.txt").exists(),
        "{why}: escaped"
    );
}

#[tokio::test]
async fn path_traversal_is_rejected() {
    assert_rejected(&[("../evil.txt", EntryType::Regular, b"x", None)], "dotdot").await;
    assert_rejected(
        &[("downloads/../../evil.txt", EntryType::Regular, b"x", None)],
        "nested dotdot",
    )
    .await;
    assert_rejected(&[("/evil.txt", EntryType::Regular, b"x", None)], "absolute").await;
    assert_rejected(
        &[(
            "downloads\\..\\..\\evil.txt",
            EntryType::Regular,
            b"x",
            None,
        )],
        "backslash",
    )
    .await;
}

#[tokio::test]
async fn links_and_unexpected_members_are_rejected() {
    assert_rejected(
        &[(
            "downloads/link",
            EntryType::Symlink,
            b"",
            Some("../../evil.txt"),
        )],
        "symlink",
    )
    .await;
    assert_rejected(
        &[("downloads/hard", EntryType::Link, b"", Some("corpus.db"))],
        "hardlink",
    )
    .await;
    assert_rejected(
        &[("evil.txt", EntryType::Regular, b"x", None)],
        "unknown member",
    )
    .await;
    assert_rejected(
        &[("downloads/v1.mp4", EntryType::Regular, b"dup", None)],
        "duplicate",
    )
    .await;
}

#[tokio::test]
async fn garbage_and_missing_db_are_rejected() {
    let t = TestApp::new().await;
    let admin = t.setup_admin().await;
    let r = t
        .install(
            &admin,
            &[("file", Some("x.tar.zst"), b"definitely not zstd")],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{:?}", r.body);

    let mut b = tar::Builder::new(Vec::new());
    raw_entry(&mut b, "downloads/a.mp4", EntryType::Regular, b"x", None);
    let z = zstd::encode_all(&b.into_inner().unwrap()[..], 3).unwrap();
    let r = t.install(&admin, &[("file", Some("x.tar.zst"), &z)]).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{:?}", r.body);

    let mut b = tar::Builder::new(Vec::new());
    raw_entry(
        &mut b,
        "corpus.db",
        EntryType::Regular,
        b"not sqlite at all",
        None,
    );
    let z = zstd::encode_all(&b.into_inner().unwrap()[..], 3).unwrap();
    let r = t.install(&admin, &[("file", Some("x.tar.zst"), &z)]).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{:?}", r.body);

    let r = t.install(&admin, &[("name", None, b"x")]).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(entries(&t).is_empty(), "{:?}", entries(&t));
}
