mod common;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use common::*;
use serde_json::json;
use ytp_auth::Permission;
use ytp_server::routes::{self, Access};

#[tokio::test]
async fn health_is_public() {
    let t = TestApp::new().await;
    let r = t.get("/api/health", &Auth::None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["ok"], true);
    assert!(r.body["version"].is_string());
}

#[tokio::test]
async fn permission_catalogue() {
    let t = TestApp::new().await;
    let admin = t.setup_admin().await;
    let guest = t.create_user(&admin, "g", &["guest"]).await;
    assert_eq!(
        t.get("/api/auth/permissions", &Auth::None).await.status,
        StatusCode::UNAUTHORIZED
    );
    let r = t.get("/api/auth/permissions", &guest).await;
    assert_eq!(r.status, StatusCode::OK);
    let ids: Vec<String> = serde_json::from_value(r.body).unwrap();
    assert_eq!(ids.len(), Permission::ALL.len());
    assert!(ids.contains(&"audit.view".to_string()));
}

#[tokio::test]
async fn setup_flow() {
    let t = TestApp::new().await;
    let s = t.get("/api/auth/state", &Auth::None).await;
    assert_eq!(s.status, StatusCode::OK);
    assert_eq!(s.body["setup_required"], true);
    assert!(s.body["user"].is_null());
    assert!(s.body["csrf"].is_null());
    assert_eq!(s.body["permissions"], json!([]));

    let bad = t
        .post(
            "/api/auth/setup",
            &Auth::None,
            json!({ "token": "wrong", "username": "admin", "password": ADMIN_PW }),
        )
        .await;
    assert_eq!(bad.status, StatusCode::FORBIDDEN);

    let token = t.st.setup_token().await.unwrap();
    let short = t
        .post(
            "/api/auth/setup",
            &Auth::None,
            json!({ "token": token, "username": "admin", "password": "short" }),
        )
        .await;
    assert_eq!(short.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(short.code(), "invalid");

    let admin = t.setup_admin().await;
    let s = t.get("/api/auth/state", &admin).await;
    assert_eq!(s.body["setup_required"], false);
    assert_eq!(s.body["user"]["username"], "admin");
    assert_eq!(s.body["user"]["roles"], json!(["admin"]));
    assert_eq!(
        s.body["permissions"].as_array().unwrap().len(),
        Permission::ALL.len()
    );
    assert!(s.body["csrf"].is_string());

    // The token is single-use.
    let again = t
        .post(
            "/api/auth/setup",
            &Auth::None,
            json!({ "token": token, "username": "admin2", "password": ADMIN_PW }),
        )
        .await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert!(t.st.setup_token().await.is_none());
}

#[tokio::test]
async fn login_logout_and_wrong_password() {
    let t = TestApp::new().await;
    let admin = t.setup_admin().await;
    let alice = t.create_user(&admin, "alice", &["creator"]).await;

    let wrong = t
        .post(
            "/api/auth/login",
            &Auth::None,
            json!({ "username": "alice", "password": "nope-nope-nope" }),
        )
        .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert!(common::session_cookie(&wrong.headers).is_none());
    let ghost = t
        .post(
            "/api/auth/login",
            &Auth::None,
            json!({ "username": "ghost", "password": "nope-nope-nope" }),
        )
        .await;
    assert_eq!(ghost.status, StatusCode::UNAUTHORIZED);
    assert_eq!(ghost.code(), wrong.code());

    let s = t.get("/api/auth/state", &alice).await;
    assert_eq!(s.body["user"]["username"], "alice");
    assert!(s.body["permissions"]
        .as_array()
        .unwrap()
        .contains(&json!("generate.ytp")));

    // Logging in again from the same browser rotates the session id.
    let Auth::Session { cookie: old, .. } = &alice else {
        unreachable!()
    };
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, format!("ytp_session={old}"))
        .body(Body::from(
            json!({ "username": "alice", "password": "user-password-1" }).to_string(),
        ))
        .unwrap();
    let r = t.send(req).await;
    assert_eq!(r.status, StatusCode::OK);
    let new_cookie = common::session_cookie(&r.headers).unwrap();
    assert_ne!(&new_cookie, old);
    let s = t.get("/api/auth/state", &alice).await;
    assert!(s.body["user"].is_null(), "old session must be gone");
    let alice = Auth::Session {
        cookie: new_cookie,
        csrf: r.body["csrf"].as_str().unwrap().into(),
    };

    let Auth::Session { cookie, .. } = &alice else {
        unreachable!()
    };
    let no_csrf = t
        .call(
            Method::POST,
            "/api/auth/logout",
            &Auth::CookieOnly(cookie.clone()),
            None,
        )
        .await;
    assert_eq!(no_csrf.status, StatusCode::FORBIDDEN);
    assert_eq!(no_csrf.code(), "csrf_failed");

    let out = t.call(Method::POST, "/api/auth/logout", &alice, None).await;
    assert_eq!(out.status, StatusCode::NO_CONTENT);
    let set = out
        .headers
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(set.contains("Max-Age=0"));
    let s = t.get("/api/auth/state", &alice).await;
    assert!(s.body["user"].is_null());
    assert_eq!(
        t.get("/api/corpora", &alice).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn csrf_is_enforced_for_cookie_sessions() {
    let t = TestApp::new().await;
    let admin = t.setup_admin().await;
    let Auth::Session { cookie, .. } = &admin else {
        unreachable!()
    };
    let body = json!({ "username": "bob", "password": "bob-password", "roles": [] });

    let r = t
        .post(
            "/api/admin/users",
            &Auth::CookieOnly(cookie.clone()),
            body.clone(),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.code(), "csrf_failed");

    let forged = Auth::Session {
        cookie: cookie.clone(),
        csrf: "forged".into(),
    };
    let r = t.post("/api/admin/users", &forged, body.clone()).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.code(), "csrf_failed");

    // GETs need no token.
    let r = t
        .get("/api/admin/users", &Auth::CookieOnly(cookie.clone()))
        .await;
    assert_eq!(r.status, StatusCode::OK);

    let r = t.post("/api/admin/users", &admin, body).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["username"], "bob");
}

fn concrete(path: &str) -> String {
    path.replace("{id}", "999999")
        .replace("{name}", "nope")
        .replace("{*rest}", "x")
}

fn methods_for(spec: &routes::RouteSpec) -> Vec<Method> {
    match &spec.method {
        Some(m) => vec![m.clone()],
        None => vec![Method::GET, Method::POST, Method::DELETE],
    }
}

/// Routes reachable without any credentials. Everything else in the table
/// must demand authentication; this list is the review point.
const PUBLIC: &[&str] = &[
    "/api/health",
    "/api/auth/state",
    "/api/auth/setup",
    "/api/auth/login",
];

#[tokio::test]
async fn route_table_every_route_declares_access() {
    let t = TestApp::new().await;
    let (_, specs) = routes::api(&t.st);
    assert!(specs.len() >= 25);
    for s in &specs {
        assert!(s.path.starts_with("/api/"), "{}", s.path);
        let public = matches!(
            s.access,
            Access::Public | Access::Optional | Access::SetupToken
        );
        assert_eq!(
            public,
            PUBLIC.contains(&s.path),
            "{} {:?} has unexpected access {:?}",
            s.path,
            s.method,
            s.access
        );
    }
    // Every permission-gated area of the contract is covered.
    for p in [
        Permission::UsersManage,
        Permission::RolesManage,
        Permission::AuditView,
        Permission::TokensCreate,
        Permission::CorpusRead,
        Permission::CorpusInstall,
        Permission::CorpusDelete,
        Permission::GenerateYtp,
        Permission::GenerateYtpmv,
    ] {
        assert!(
            specs.iter().any(|s| s.access == Access::Perm(p)),
            "no route requires {p}"
        );
    }
}

#[tokio::test]
async fn every_protected_route_rejects_anonymous_and_unprivileged() {
    let t = TestApp::new().await;
    let admin = t.setup_admin().await;
    let guest = t.create_user(&admin, "guest1", &["guest"]).await;
    let guest_perms: Vec<Permission> = {
        let r = t.get("/api/auth/state", &guest).await;
        serde_json::from_value(r.body["permissions"].clone()).unwrap()
    };
    assert!(!guest_perms.contains(&Permission::UsersManage));

    let (_, specs) = routes::api(&t.st);
    let mut checked_403 = 0;
    for s in &specs {
        if PUBLIC.contains(&s.path) {
            continue;
        }
        let path = concrete(s.path);
        for m in methods_for(s) {
            let anon = t.call(m.clone(), &path, &Auth::None, None).await;
            assert_eq!(
                anon.status,
                StatusCode::UNAUTHORIZED,
                "anonymous {m} {path} -> {} {:?}",
                anon.status,
                anon.body
            );
            assert_eq!(anon.code(), "unauthorized");

            let needed = match s.access {
                Access::Perm(p) => Some(p),
                Access::ReadWrite { read, write } => {
                    Some(if m == Method::GET { read } else { write })
                }
                _ => None,
            };
            if let Some(p) = needed.filter(|p| !guest_perms.contains(p)) {
                let r = t.call(m.clone(), &path, &guest, None).await;
                assert_eq!(
                    r.status,
                    StatusCode::FORBIDDEN,
                    "guest {m} {path} (needs {p}) -> {} {:?}",
                    r.status,
                    r.body
                );
                assert_eq!(r.code(), "forbidden", "{m} {path}");
                checked_403 += 1;
            }
        }
    }
    assert!(checked_403 >= 15, "only {checked_403} 403 checks ran");
}

#[tokio::test]
async fn api_tokens_authenticate_and_are_scoped() {
    let t = TestApp::new().await;
    let admin = t.setup_admin().await;

    let r = t
        .post(
            "/api/tokens",
            &admin,
            json!({ "name": "ci", "permissions": ["corpus.read", "corpus.delete"], "expires_days": 30 }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let secret = r.body["secret"].as_str().unwrap().to_string();
    assert!(secret.starts_with("ytp_"));
    assert_eq!(
        r.body["token"]["permissions"],
        json!(["corpus.read", "corpus.delete"])
    );
    assert!(r.body["token"]["expires_at"].is_string());
    let id = r.body["token"]["id"].as_i64().unwrap();
    let tok = Auth::Bearer(secret.clone());

    assert_eq!(t.get("/api/corpora", &tok).await.status, StatusCode::OK);
    // Outside its scope, even though its owner is admin.
    let r = t.get("/api/admin/users", &tok).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = t
        .post(
            "/api/tokens",
            &tok,
            json!({ "name": "x", "permissions": ["corpus.read"] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // Bearer requests are CSRF-exempt: this reaches the handler (404).
    let r = t
        .call(Method::DELETE, "/api/corpora/nope", &tok, None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{:?}", r.body);
    // Session-only endpoints refuse tokens.
    let r = t.call(Method::POST, "/api/auth/logout", &tok, None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // The token's state shows its narrowed permissions and no CSRF.
    let s = t.get("/api/auth/state", &tok).await;
    assert_eq!(
        s.body["permissions"],
        json!(["corpus.read", "corpus.delete"])
    );
    assert!(s.body["csrf"].is_null());

    // last_used_at is recorded.
    let list = t.get("/api/tokens", &admin).await;
    assert!(list.body[0]["last_used_at"].is_string());
    assert!(list.body[0].get("secret").is_none());

    // Bad bearer: 401.
    let r = t
        .get("/api/corpora", &Auth::Bearer("ytp_nope".into()))
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    // Revocation.
    let r = t
        .call(Method::DELETE, &format!("/api/tokens/{id}"), &admin, None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(
        t.get("/api/corpora", &tok).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn token_cannot_exceed_creator_permissions() {
    let t = TestApp::new().await;
    let admin = t.setup_admin().await;
    let creator = t.create_user(&admin, "carol", &["creator"]).await;
    let r = t
        .post(
            "/api/tokens",
            &creator,
            json!({ "name": "sneaky", "permissions": ["users.manage"] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = t
        .post(
            "/api/tokens",
            &creator,
            json!({ "name": "x", "permissions": ["bogus.perm"] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = t
        .post(
            "/api/tokens",
            &creator,
            json!({ "name": "ok", "permissions": ["corpus.read"] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);

    // Another user's token is invisible and undeletable.
    let id = r.body["token"]["id"].as_i64().unwrap();
    let r = t
        .call(Method::DELETE, &format!("/api/tokens/{id}"), &admin, None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(t.get("/api/tokens", &admin).await.body, json!([]));
}

#[tokio::test]
async fn not_implemented_stubs() {
    let t = TestApp::new().await;
    let admin = t.setup_admin().await;
    for (m, p) in [
        (Method::POST, "/api/ytp/generate"),
        (Method::POST, "/api/ytpmv/render"),
        (Method::GET, "/api/ytpmv/presets"),
        (Method::GET, "/api/jobs/abc"),
        (Method::GET, "/api/projects"),
        (Method::POST, "/api/projects"),
        (Method::GET, "/api/projects/1"),
        (Method::PATCH, "/api/projects/1"),
    ] {
        let r = t.call(m.clone(), p, &admin, Some(json!({}))).await;
        assert_eq!(r.status, StatusCode::NOT_IMPLEMENTED, "{m} {p}");
        assert_eq!(r.code(), "not_implemented");
    }
}

#[tokio::test]
async fn login_is_rate_limited() {
    let t = TestApp::new().await;
    t.setup_admin().await;
    let body = json!({ "username": "admin", "password": "wrong-password" });
    for _ in 0..10 {
        let r = t.post("/api/auth/login", &Auth::None, body.clone()).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    let r = t.post("/api/auth/login", &Auth::None, body).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.code(), "rate_limited");
    // Even the right password is refused while limited.
    let r = t
        .post(
            "/api/auth/login",
            &Auth::None,
            json!({ "username": "admin", "password": ADMIN_PW }),
        )
        .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn admin_users_roles_audit() {
    let t = TestApp::new().await;
    let admin = t.setup_admin().await;
    let _bob = t.create_user(&admin, "bob", &["editor"]).await;

    let users = t.get("/api/admin/users", &admin).await;
    assert_eq!(users.body.as_array().unwrap().len(), 2);
    let bob_id = users.body[1]["id"].as_i64().unwrap();
    let admin_id = users.body[0]["id"].as_i64().unwrap();

    // Duplicate username.
    let r = t
        .post(
            "/api/admin/users",
            &admin,
            json!({ "username": "BOB", "password": "whatever-1", "roles": [] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    // Unknown role.
    let r = t
        .post(
            "/api/admin/users",
            &admin,
            json!({ "username": "zed", "password": "whatever-1", "roles": ["wizard"] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);

    // Disabling bob kills his session.
    let r = t
        .call(
            Method::PATCH,
            &format!("/api/admin/users/{bob_id}"),
            &admin,
            Some(json!({ "disabled": true })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["disabled"], true);
    assert_eq!(
        t.get("/api/corpora", &_bob).await.status,
        StatusCode::UNAUTHORIZED
    );

    // The last admin can neither be demoted nor delete themselves.
    let r = t
        .call(
            Method::PATCH,
            &format!("/api/admin/users/{admin_id}"),
            &admin,
            Some(json!({ "roles": ["guest"] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let r = t
        .call(
            Method::DELETE,
            &format!("/api/admin/users/{admin_id}"),
            &admin,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);

    // Roles.
    let roles = t.get("/api/admin/roles", &admin).await;
    assert_eq!(roles.body.as_array().unwrap().len(), 4);
    assert!(roles.body[0]["quota"]["max_concurrent_jobs"].is_number());
    let r = t
        .post(
            "/api/admin/roles",
            &admin,
            json!({ "name": "viewer", "permissions": ["corpus.read"] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["builtin"], false);
    let r = t
        .call(
            Method::PATCH,
            "/api/admin/roles/viewer",
            &admin,
            Some(json!({ "permissions": ["corpus.read", "project.read"] })),
        )
        .await;
    assert_eq!(
        r.body["permissions"],
        json!(["project.read", "corpus.read"])
    );
    let r = t
        .call(Method::DELETE, "/api/admin/roles/guest", &admin, None)
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let r = t
        .call(Method::DELETE, "/api/admin/roles/viewer", &admin, None)
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);

    // Audit: newest first, paginated by id.
    let a = t.get("/api/admin/audit?limit=3", &admin).await;
    let entries = a.body.as_array().unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0]["action"], "role.delete");
    let oldest = entries[2]["id"].as_i64().unwrap();
    let b = t
        .get(
            &format!("/api/admin/audit?limit=100&before={oldest}"),
            &admin,
        )
        .await;
    assert!(b
        .body
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["id"].as_i64().unwrap() < oldest));
    assert!(b
        .body
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["action"] == "auth.setup"));
}

#[tokio::test]
async fn password_change() {
    let t = TestApp::new().await;
    let admin = t.setup_admin().await;
    let r = t
        .post(
            "/api/auth/password",
            &admin,
            json!({ "current": "wrong", "new": "new-password-1" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let other = t.login("admin", ADMIN_PW).await;
    let r = t
        .post(
            "/api/auth/password",
            &admin,
            json!({ "current": ADMIN_PW, "new": "new-password-1" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    // This session survives, the other is signed out.
    assert_eq!(
        t.get("/api/admin/users", &admin).await.status,
        StatusCode::OK
    );
    assert_eq!(
        t.get("/api/admin/users", &other).await.status,
        StatusCode::UNAUTHORIZED
    );
    t.login("admin", "new-password-1").await;
}

#[tokio::test]
async fn unknown_api_paths_are_json_404_and_spa_fallback() {
    let t = TestApp::new().await;
    let r = t.get("/api/does-not-exist", &Auth::None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.code(), "not_found");
    // A client-side route gets the SPA (or the "not built" page): never JSON.
    let r = t.get("/setup?token=abc", &Auth::None).await;
    assert!(r.body.is_string());
    if !ytp_server::web::has_ui() {
        assert_eq!(r.status, StatusCode::NOT_FOUND);
    } else {
        assert_eq!(r.status, StatusCode::OK);
    }
    assert_eq!(r.headers.get("referrer-policy").unwrap(), "no-referrer");
}
