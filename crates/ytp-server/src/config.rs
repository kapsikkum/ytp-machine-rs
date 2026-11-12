use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub bind: SocketAddr,
    /// ffmpeg binary used for rendering (default: `ffmpeg` on PATH).
    pub ffmpeg: Option<PathBuf>,
    /// How many jobs may render at once, across all users.
    pub job_workers: usize,
    /// Extra origins allowed to open the WebSocket (e.g. a Vite dev server).
    pub allowed_origins: Vec<String>,
    /// Trust `X-Forwarded-For` / `X-Forwarded-Host` from a reverse proxy.
    pub trust_proxy: bool,
    /// Always mark the session cookie `Secure` (it is also set whenever the
    /// request arrived with `X-Forwarded-Proto: https`).
    pub secure_cookies: bool,
}

impl Config {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            bind: ([127, 0, 0, 1], 8765).into(),
            ffmpeg: None,
            job_workers: 2,
            allowed_origins: Vec::new(),
            trust_proxy: false,
            secure_cookies: false,
        }
    }
}
