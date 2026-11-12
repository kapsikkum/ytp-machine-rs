//! A small shared connection status, readable from the server's admin API.

use std::sync::Mutex;

use crate::backend::BotStatus;

#[derive(Debug, Default)]
pub struct StatusHandle {
    inner: Mutex<BotStatus>,
}

impl StatusHandle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self) -> BotStatus {
        self.inner.lock().unwrap().clone()
    }

    pub fn set(&self, connected: bool, error: Option<String>) {
        let mut g = self.inner.lock().unwrap();
        let now = ytp_auth::rfc3339(ytp_auth::now());
        if g.connected != connected || g.last_error != error {
            g.since = Some(now);
        }
        g.connected = connected;
        g.last_error = error;
    }

    pub fn connected(&self) {
        self.set(true, None);
    }

    pub fn failed(&self, error: impl Into<String>) {
        self.set(false, Some(error.into()));
    }
}
