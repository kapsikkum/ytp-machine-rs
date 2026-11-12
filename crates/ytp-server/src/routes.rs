//! The API route table. Every route is registered through [`ApiBuilder`],
//! which forces an [`Access`] declaration and wraps the handler in the auth
//! guard, so a route without a permission check cannot be written.

use axum::extract::{DefaultBodyLimit, Request, State};
use axum::handler::Handler;
use axum::http::Method;
use axum::middleware::{from_fn_with_state, Next};
use axum::routing::{on, MethodFilter, MethodRouter};
use axum::Router;
use ytp_auth::Permission::{self, *};

use crate::handlers::{admin, auth, corpora, encode, jobs, misc, projects, tokens, ytpmv};
use crate::{ws, AppState};

/// Who may call a route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Anyone; the caller is not even looked up.
    Public,
    /// Anyone; the caller is resolved if credentials are present.
    Optional,
    /// Anyone holding the one-time first-run setup token (checked in handler).
    SetupToken,
    /// A cookie session (not an API token).
    Session,
    /// Any authenticated caller.
    Authenticated,
    Perm(Permission),
    /// `read` for GET/HEAD, `write` for everything else.
    ReadWrite {
        read: Permission,
        write: Permission,
    },
}

#[derive(Debug, Clone)]
pub struct RouteSpec {
    /// `None` = any method.
    pub method: Option<Method>,
    pub path: &'static str,
    pub access: Access,
}

pub struct ApiBuilder<'a> {
    state: &'a AppState,
    router: Router<AppState>,
    specs: Vec<RouteSpec>,
}

impl<'a> ApiBuilder<'a> {
    pub fn new(state: &'a AppState) -> Self {
        Self {
            state,
            router: Router::new(),
            specs: Vec::new(),
        }
    }

    fn add<H, T>(
        mut self,
        method: Option<Method>,
        path: &'static str,
        access: Access,
        handler: H,
        body_limit: Option<usize>,
    ) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        let mr: MethodRouter<AppState> = match &method {
            Some(m) => on(
                MethodFilter::try_from(m.clone()).expect("supported method"),
                handler,
            ),
            // Not `any()`: that registers a fallback, which route_layer skips.
            None => on(
                MethodFilter::GET
                    .or(MethodFilter::HEAD)
                    .or(MethodFilter::POST)
                    .or(MethodFilter::PUT)
                    .or(MethodFilter::PATCH)
                    .or(MethodFilter::DELETE)
                    .or(MethodFilter::OPTIONS),
                handler,
            ),
        };
        let mr = mr.route_layer(from_fn_with_state(
            self.state.clone(),
            move |State(st): State<AppState>, req: Request, next: Next| {
                crate::auth::guard(st, access, req, next)
            },
        ));
        let mr = match body_limit {
            Some(n) => mr.layer(DefaultBodyLimit::max(n)),
            None => mr,
        };
        self.router = self.router.route(path, mr);
        self.specs.push(RouteSpec {
            method,
            path,
            access,
        });
        self
    }

    pub fn get<H: Handler<T, AppState>, T: 'static>(
        self,
        p: &'static str,
        a: Access,
        h: H,
    ) -> Self {
        self.add(Some(Method::GET), p, a, h, None)
    }

    pub fn post<H: Handler<T, AppState>, T: 'static>(
        self,
        p: &'static str,
        a: Access,
        h: H,
    ) -> Self {
        self.add(Some(Method::POST), p, a, h, None)
    }

    pub fn patch<H: Handler<T, AppState>, T: 'static>(
        self,
        p: &'static str,
        a: Access,
        h: H,
    ) -> Self {
        self.add(Some(Method::PATCH), p, a, h, None)
    }

    pub fn delete<H: Handler<T, AppState>, T: 'static>(
        self,
        p: &'static str,
        a: Access,
        h: H,
    ) -> Self {
        self.add(Some(Method::DELETE), p, a, h, None)
    }

    pub fn any<H: Handler<T, AppState>, T: 'static>(
        self,
        p: &'static str,
        a: Access,
        h: H,
    ) -> Self {
        self.add(None, p, a, h, None)
    }

    pub fn post_large<H: Handler<T, AppState>, T: 'static>(
        self,
        p: &'static str,
        a: Access,
        h: H,
        limit: usize,
    ) -> Self {
        self.add(Some(Method::POST), p, a, h, Some(limit))
    }

    pub fn patch_large<H: Handler<T, AppState>, T: 'static>(
        self,
        p: &'static str,
        a: Access,
        h: H,
        limit: usize,
    ) -> Self {
        self.add(Some(Method::PATCH), p, a, h, Some(limit))
    }

    pub fn finish(self) -> (Router<AppState>, Vec<RouteSpec>) {
        (self.router, self.specs)
    }
}

pub const MAX_BUNDLE_BYTES: usize = 8 << 30;

/// Every `/api` route.
pub fn api(state: &AppState) -> (Router<AppState>, Vec<RouteSpec>) {
    use Access::*;
    ApiBuilder::new(state)
        .get("/api/health", Public, misc::health)
        // auth
        .get("/api/auth/state", Optional, auth::state)
        .post("/api/auth/setup", SetupToken, auth::setup)
        .post("/api/auth/login", Public, auth::login)
        .post("/api/auth/logout", Session, auth::logout)
        .post("/api/auth/password", Session, auth::change_password)
        .get("/api/auth/permissions", Authenticated, auth::permissions)
        // admin
        .get("/api/admin/users", Perm(UsersManage), admin::list_users)
        .post("/api/admin/users", Perm(UsersManage), admin::create_user)
        .patch(
            "/api/admin/users/{id}",
            Perm(UsersManage),
            admin::update_user,
        )
        .delete(
            "/api/admin/users/{id}",
            Perm(UsersManage),
            admin::delete_user,
        )
        .get("/api/admin/roles", Perm(RolesManage), admin::list_roles)
        .post("/api/admin/roles", Perm(RolesManage), admin::create_role)
        .patch(
            "/api/admin/roles/{name}",
            Perm(RolesManage),
            admin::update_role,
        )
        .delete(
            "/api/admin/roles/{name}",
            Perm(RolesManage),
            admin::delete_role,
        )
        .get("/api/admin/audit", Perm(AuditView), admin::audit)
        .get("/api/admin/encoders", Perm(SettingsManage), encode::encoders)
        .get("/api/admin/bots", Perm(SettingsManage), crate::bots::status)
        // tokens
        .get("/api/tokens", Perm(TokensCreate), tokens::list)
        .post("/api/tokens", Perm(TokensCreate), tokens::create)
        .delete("/api/tokens/{id}", Perm(TokensCreate), tokens::delete)
        // corpora
        .get("/api/corpora", Perm(CorpusRead), corpora::list)
        .post_large(
            "/api/corpora/install",
            Perm(CorpusInstall),
            corpora::install,
            MAX_BUNDLE_BYTES,
        )
        .delete("/api/corpora/{name}", Perm(CorpusDelete), corpora::delete)
        // events
        .get("/api/ws", Authenticated, ws::handler)
        // later phases: authenticated and permission-checked, then 501
        // jobs: generation is gated by permission; a job is then its owner's
        // (or visible to jobs.view_all), which the handlers check
        .post("/api/ytp/generate", Perm(GenerateYtp), jobs::generate)
        .get("/api/jobs/{id}", Authenticated, jobs::get)
        .delete("/api/jobs/{id}", Authenticated, jobs::cancel)
        .get("/api/jobs/{id}/result", Authenticated, jobs::result)
        // re-encode a finished job's master with another preset
        .get("/api/encode/presets", Perm(ExportRender), encode::presets)
        .post("/api/jobs/{id}/export", Perm(ExportRender), encode::export)
        // ytpmv: a MIDI library per user, previews that answer with audio,
        // and a render queued like a sentence (which also needs export.render,
        // checked in the handler)
        .post_large(
            "/api/ytpmv/midi",
            Perm(GenerateYtpmv),
            ytpmv::upload,
            crate::ytpmv::MAX_MIDI_BYTES + (64 << 10),
        )
        .get("/api/ytpmv/library", Perm(GenerateYtpmv), ytpmv::library)
        .get("/api/ytpmv/midi/{id}", Perm(GenerateYtpmv), ytpmv::get_midi)
        .patch("/api/ytpmv/midi/{id}", Perm(GenerateYtpmv), ytpmv::rename)
        .delete("/api/ytpmv/midi/{id}", Perm(GenerateYtpmv), ytpmv::delete)
        .get("/api/ytpmv/midi/{id}/notes", Perm(GenerateYtpmv), ytpmv::notes)
        .get("/api/ytpmv/midi/{id}/part/{part}", Perm(GenerateYtpmv), ytpmv::part_voice)
        .post("/api/ytpmv/sample", Perm(GenerateYtpmv), ytpmv::sample)
        .post("/api/ytpmv/sample/audio", Perm(GenerateYtpmv), ytpmv::sample_audio)
        .post("/api/ytpmv/preview", Perm(GenerateYtpmv), ytpmv::preview)
        .post("/api/ytpmv/preview-mix", Perm(GenerateYtpmv), ytpmv::preview_mix)
        .post("/api/ytpmv/render", Perm(GenerateYtpmv), ytpmv::render)
        // timeline projects: owned, shareable, played by the preview and the render job
        .get("/api/projects", Perm(ProjectRead), projects::list)
        .post_large("/api/projects", Perm(ProjectWrite), projects::create, crate::projects::MAX_PROJECT_BYTES + (1 << 20))
        .get("/api/projects/{id}", Perm(ProjectRead), projects::get)
        .patch_large("/api/projects/{id}", Perm(ProjectWrite), projects::update, crate::projects::MAX_PROJECT_BYTES + (1 << 20))
        .delete("/api/projects/{id}", Perm(ProjectWrite), projects::delete)
        .get("/api/projects/{id}/shares", Perm(ProjectShare), projects::shares)
        .post("/api/projects/{id}/shares", Perm(ProjectShare), projects::share)
        .delete("/api/projects/{id}/shares/{name}", Perm(ProjectShare), projects::unshare)
        .post("/api/projects/{id}/render", Perm(ExportRender), projects::render)
        // a generator's plan as a project, and a low-res render of a time window
        .post("/api/ytp/plan", Perm(GenerateYtp), projects::plan_ytp)
        .post("/api/ytpmv/plan", Perm(GenerateYtpmv), projects::plan_ytpmv)
        .post("/api/preview", Perm(StudioUse), projects::preview)
        .finish()
}
