//! In-memory sliding-window limiter for login attempts.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct LoginLimiter {
    window: Duration,
    per_user: usize,
    per_ip: usize,
    hits: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl Default for LoginLimiter {
    fn default() -> Self {
        Self::new(Duration::from_secs(60), 10, 30)
    }
}

impl LoginLimiter {
    pub fn new(window: Duration, per_user: usize, per_ip: usize) -> Self {
        Self {
            window,
            per_user,
            per_ip,
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// Record an attempt; `false` means it is over the limit and must be
    /// refused. Limits apply per IP+username and, more loosely, per IP alone
    /// (so one address cannot spray many usernames).
    pub fn attempt(&self, ip: &str, username: &str) -> bool {
        let now = Instant::now();
        let user_key = format!("u\0{ip}\0{}", username.to_lowercase());
        let ip_key = format!("i\0{ip}");
        let mut hits = self.hits.lock().unwrap();
        let ok_user = Self::bump(&mut hits, user_key, now, self.window, self.per_user);
        let ok_ip = Self::bump(&mut hits, ip_key, now, self.window, self.per_ip);
        ok_user && ok_ip
    }

    fn bump(
        hits: &mut HashMap<String, VecDeque<Instant>>,
        key: String,
        now: Instant,
        window: Duration,
        limit: usize,
    ) -> bool {
        let q = hits.entry(key).or_default();
        while q.front().is_some_and(|t| now.duration_since(*t) > window) {
            q.pop_front();
        }
        if q.len() >= limit {
            return false;
        }
        q.push_back(now);
        true
    }

    /// Drop stale keys; call periodically.
    pub fn prune(&self) {
        let now = Instant::now();
        let window = self.window;
        self.hits
            .lock()
            .unwrap()
            .retain(|_, q| q.back().is_some_and(|t| now.duration_since(*t) <= window));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits() {
        let l = LoginLimiter::new(Duration::from_secs(60), 3, 5);
        assert!(l.attempt("1.1.1.1", "bob"));
        assert!(l.attempt("1.1.1.1", "BOB"));
        assert!(l.attempt("1.1.1.1", "bob"));
        assert!(!l.attempt("1.1.1.1", "bob"));
        assert!(l.attempt("2.2.2.2", "bob"));
        // The refused attempt still counted against the IP: this is the 5th.
        assert!(l.attempt("1.1.1.1", "alice"));
        assert!(!l.attempt("1.1.1.1", "carol"));
    }
}
