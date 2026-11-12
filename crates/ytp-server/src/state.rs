use std::ops::Deref;
use std::sync::Arc;

use tokio::sync::{broadcast, Mutex};
use ytp_auth::{secret, AuthStore, LoginLimiter};
use ytp_core::{Event, Result};
use ytp_corpus::{CorpusStore, Limits};

use ytp_media::{ClipCache, FfmpegRenderer, Renderer};

use crate::jobs::Jobs;
use crate::throttle::Throttle;
use crate::Config;

pub struct Inner {
    pub config: Config,
    pub auth: AuthStore,
    pub corpora: CorpusStore,
    pub limiter: LoginLimiter,
    /// Per-user `requests_per_minute` on the job-creating endpoints.
    pub throttle: Throttle,
    /// One-time first-run token; `Some` only while no user exists.
    pub setup_token: Mutex<Option<String>>,
    pub events: broadcast::Sender<Event>,
    pub jobs: Jobs,
    /// Cuts and encodes video; ffmpeg unless a test supplies another.
    pub renderer: Arc<dyn Renderer>,
    /// Shared encode core: presets, hardware encoders, the final encode.
    pub encoder: Arc<ytp_encode::Encoder>,
    /// The MIDI library and what YTPMV renders are made from.
    pub ytpmv: crate::ytpmv::Service,
    /// Timeline projects: plans, project renders, previews and their caches.
    pub projects: crate::projects::Service,
    /// What the chat bots report (empty until `bots::start` runs).
    pub bots: crate::bots::BotRegistry,
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
        let ffmpeg: std::path::PathBuf = config.ffmpeg.clone().unwrap_or_else(|| "ffmpeg".into());
        let mut renderer = FfmpegRenderer::new(ffmpeg.clone());
        if config.clip_cache_mb > 0 {
            let dir = config.data_dir.join("cache").join("clips");
            renderer = renderer.with_cache(ClipCache::new(dir, config.clip_cache_mb << 20, ffmpeg));
        }
        Self::with_renderer(config, Arc::new(renderer)).await
    }

    pub async fn with_renderer(config: Config, renderer: Arc<dyn Renderer>) -> Result<Self> {
        std::fs::create_dir_all(&config.data_dir)?;
        let ffmpeg = config.ffmpeg.clone().unwrap_or_else(|| "ffmpeg".into());
        let (presets, preset_problems) =
            ytp_encode::PresetSet::with_dir(&config.data_dir.join("presets"));
        for problem in &preset_problems {
            tracing::warn!(problem = %problem, "a preset file was skipped");
        }
        let encoder = Arc::new(ytp_encode::Encoder::with_presets(ffmpeg.clone(), presets));
        let auth = AuthStore::open(&config.data_dir.join("app.db")).await?;
        let corpora = CorpusStore::open(config.data_dir.join("corpora"), Limits::default())?;
        let setup_token = if auth.user_count().await? == 0 {
            Some(secret::random_secret())
        } else {
            None
        };
        let (events, _) = broadcast::channel(256);
        let jobs = Jobs::new(&config.data_dir, config.job_workers, events.clone());
        let projects = crate::projects::Service::new(&config.data_dir, ffmpeg.clone(), encoder.clone());
        let ytpmv = crate::ytpmv::Service::new(&config.data_dir, ffmpeg);
        let state = Self(Arc::new(Inner {
            config,
            auth,
            corpora,
            limiter: LoginLimiter::default(),
            throttle: Throttle::default(),
            setup_token: Mutex::new(setup_token),
            events,
            jobs,
            renderer,
            encoder,
            ytpmv,
            projects,
            bots: crate::bots::BotRegistry::default(),
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
                st.throttle.prune();
            }
        });
    }
}
