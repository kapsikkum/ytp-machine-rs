use std::ops::Deref;
use std::sync::Arc;

use tokio::sync::{broadcast, Mutex};
use ytp_auth::{secret, AuthStore, LoginLimiter};
use ytp_core::{Event, Result};
use ytp_corpus::{CorpusStore, Limits};

use ytp_media::{FfmpegRenderer, Renderer};

use crate::jobs::Jobs;
use crate::Config;

pub struct Inner {
    pub config: Config,
    pub auth: AuthStore,
    pub corpora: CorpusStore,
    pub limiter: LoginLimiter,
    /// One-time first-run token; `Some` only while no user exists.
    pub setup_token: Mutex<Option<String>>,
    pub events: broadcast::Sender<Event>,
    pub jobs: Jobs,
    /// Cuts and encodes video; ffmpeg unless a test supplies another.
    pub renderer: Arc<dyn Renderer>,
}

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

impl Deref for AppState {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl AppState {
    pub async fn new(config: Config) -> Result<Self> {
        let ffmpeg = config.ffmpeg.clone().unwrap_or_else(|| "ffmpeg".into());
        Self::with_renderer(config, Arc::new(FfmpegRenderer::new(ffmpeg))).await
    }

    pub async fn with_renderer(config: Config, renderer: Arc<dyn Renderer>) -> Result<Self> {
        std::fs::create_dir_all(&config.data_dir)?;
        let auth = AuthStore::open(&config.data_dir.join("app.db")).await?;
        let corpora = CorpusStore::open(config.data_dir.join("corpora"), Limits::default())?;
        let setup_token = if auth.user_count().await? == 0 {
            Some(secret::random_secret())
        } else {
            None
        };
        let (events, _) = broadcast::channel(256);
        let jobs = Jobs::new(&config.data_dir, config.job_workers, events.clone());
        let state = Self(Arc::new(Inner {
            config,
            auth,
            corpora,
            limiter: LoginLimiter::default(),
            setup_token: Mutex::new(setup_token),
            events,
            jobs,
            renderer,
        }));
        crate::jobs::start_workers(&state);
        Ok(state)
    }

    pub async fn setup_token(&self) -> Option<String> {
        self.setup_token.lock().await.clone()
    }

    /// Publish an event to WebSocket subscribers (no-op if none).
    pub fn publish(&self, event: Event) {
        let _ = self.events.send(event);
    }

    /// Housekeeping: expired sessions, stale rate-limit buckets.
    pub fn spawn_maintenance(&self) {
        let st = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(600));
            loop {
                tick.tick().await;
                if let Err(e) = st.auth.purge_expired().await {
                    tracing::warn!(error = %e, "session purge failed");
                }
                st.limiter.prune();
            }
        });
    }
}
