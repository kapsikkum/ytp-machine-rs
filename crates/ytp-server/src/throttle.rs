//! Per-user request quota. The implementation lives in `ytp-auth` so the chat
//! bots can rate-limit their own users with the same semantics; this module
//! keeps the server's import path.

pub use ytp_auth::throttle::*;
