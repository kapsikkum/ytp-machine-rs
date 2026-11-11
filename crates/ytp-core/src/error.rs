use thiserror::Error;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Domain error. Each variant maps onto one HTTP status in the server.
#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    BadRequest(String),
    #[error("authentication required")]
    Unauthorized,
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    /// Well-formed request that fails validation (HTTP 422).
    #[error("{0}")]
    Invalid(String),
    #[error("too many requests")]
    RateLimited,
    #[error("payload too large: {0}")]
    TooLarge(String),
    #[error("not implemented")]
    NotImplemented,
    #[error("internal error: {0}")]
    Internal(String),
}

impl Error {
    pub fn internal(e: impl std::fmt::Display) -> Self {
        Error::Internal(e.to_string())
    }

    pub fn invalid(msg: impl Into<String>) -> Self {
        Error::Invalid(msg.into())
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Error::NotFound(msg.into())
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Error::Conflict(msg.into())
    }

    pub fn forbidden(msg: impl Into<String>) -> Self {
        Error::Forbidden(msg.into())
    }

    pub fn bad_request(msg: impl Into<String>) -> Self {
        Error::BadRequest(msg.into())
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Internal(format!("io: {e}"))
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Internal(format!("json: {e}"))
    }
}

#[cfg(feature = "sqlx")]
impl From<sqlx::Error> for Error {
    fn from(e: sqlx::Error) -> Self {
        match &e {
            sqlx::Error::RowNotFound => Error::NotFound("not found".into()),
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                Error::Conflict("already exists".into())
            }
            _ => Error::Internal(format!("database: {e}")),
        }
    }
}
