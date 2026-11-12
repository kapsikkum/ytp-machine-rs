//! Request quota: a token bucket holding a minute's worth of requests
//! (`requests_per_minute`), refilled evenly.
//!
//! The server keys buckets by user id; bots key them by chat user, so the same
//! semantics apply to a Discord member or a Matrix user.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq, Eq)]
pub enum Throttled {
    /// The quota is 0: the role may not make these requests at all.
    Off,
    /// Out of tokens; one is back after this many seconds.
    Wait(u64),
}

struct Bucket {
    tokens: f64,
    at: Instant,
}

#[derive(Default)]
pub struct Throttle {
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl Throttle {
    pub fn take(&self, user: i64, per_minute: u32) -> Result<(), Throttled> {
        self.take_key(&user.to_string(), per_minute)
    }

    /// Spend one token of `user`'s bucket, as of `now`.
    pub fn take_at(&self, user: i64, per_minute: u32, now: Instant) -> Result<(), Throttled> {
        self.take_key_at(&user.to_string(), per_minute, now)
    }

    /// Spend one token of an arbitrary key's bucket.
    pub fn take_key(&self, key: &str, per_minute: u32) -> Result<(), Throttled> {
        self.take_key_at(key, per_minute, Instant::now())
    }

    /// Spend one token of `key`'s bucket, as of `now`.
    pub fn take_key_at(&self, key: &str, per_minute: u32, now: Instant) -> Result<(), Throttled> {
        if per_minute == 0 {
            return Err(Throttled::Off);
        }
        let cap = per_minute as f64;
        let rate = cap / 60.0;
        let mut g = self.buckets.lock().unwrap();
        let b = g.entry(key.to_string()).or_insert(Bucket { tokens: cap, at: now });
        let elapsed = now.saturating_duration_since(b.at).as_secs_f64();
        // A changed quota is picked up at once: never hold more than the cap.
        b.tokens = (b.tokens + elapsed * rate).min(cap);
        b.at = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            Ok(())
        } else {
            let wait = ((1.0 - b.tokens) / rate).ceil().max(1.0);
            Err(Throttled::Wait(wait as u64))
        }
    }

    /// Forget buckets that have been idle long enough to be full again.
    pub fn prune(&self) {
        let now = Instant::now();
        self.buckets
            .lock()
            .unwrap()
            .retain(|_, b| now.saturating_duration_since(b.at) < Duration::from_secs(120));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_up_to_the_quota_then_a_wait() {
        let t = Throttle::default();
        let t0 = Instant::now();
        for _ in 0..30 {
            assert_eq!(t.take_at(1, 30, t0), Ok(()));
        }
        // 30 a minute is one every two seconds
        assert_eq!(t.take_at(1, 30, t0), Err(Throttled::Wait(2)));
        assert_eq!(t.take_at(1, 30, t0 + Duration::from_secs(1)), Err(Throttled::Wait(1)));
        assert_eq!(t.take_at(1, 30, t0 + Duration::from_secs(2)), Ok(()));
        assert_eq!(t.take_at(1, 30, t0 + Duration::from_secs(2)), Err(Throttled::Wait(2)));
    }

    #[test]
    fn users_do_not_share_a_bucket_and_refill_is_capped() {
        let t = Throttle::default();
        let t0 = Instant::now();
        assert_eq!(t.take_at(1, 1, t0), Ok(()));
        assert_eq!(t.take_at(1, 1, t0), Err(Throttled::Wait(60)));
        assert_eq!(t.take_at(2, 1, t0), Ok(()));
        // an hour later the bucket holds one token, not sixty
        let later = t0 + Duration::from_secs(3600);
        assert_eq!(t.take_at(1, 1, later), Ok(()));
        assert!(t.take_at(1, 1, later).is_err());
    }

    #[test]
    fn zero_means_off_and_a_lowered_quota_applies_at_once() {
        let t = Throttle::default();
        let t0 = Instant::now();
        assert_eq!(t.take_at(1, 0, t0), Err(Throttled::Off));
        assert_eq!(t.take_at(1, 100, t0), Ok(()));
        // role edited down to 2/min: the 99 tokens left are cut to 2
        assert_eq!(t.take_at(1, 2, t0), Ok(()));
        assert_eq!(t.take_at(1, 2, t0), Ok(()));
        assert!(matches!(t.take_at(1, 2, t0), Err(Throttled::Wait(_))));
    }

    #[test]
    fn plain_keys_share_the_same_buckets() {
        let t = Throttle::default();
        let t0 = Instant::now();
        assert_eq!(t.take_key_at("@a:example.org", 1, t0), Ok(()));
        assert!(matches!(t.take_key_at("@a:example.org", 1, t0), Err(Throttled::Wait(_))));
        assert_eq!(t.take_key_at("@b:example.org", 1, t0), Ok(()));
    }
}
