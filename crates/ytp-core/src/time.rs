/// Current time as unix seconds.
pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Unix seconds as RFC 3339 (`2026-01-02T03:04:05Z`).
pub fn rfc3339(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
