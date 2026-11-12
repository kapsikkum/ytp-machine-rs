//! Bot errors split into the ones a chat user should see and the ones only the
//! log should.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotError {
    /// Something the person who asked should be told, without a stack trace.
    User(String),
    /// A broken backend, network or configuration problem.
    Internal(String),
}

impl BotError {
    pub fn user(msg: impl Into<String>) -> Self {
        BotError::User(msg.into())
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        BotError::Internal(msg.into())
    }

    pub fn message(&self) -> &str {
        match self {
            BotError::User(m) | BotError::Internal(m) => m,
        }
    }
}

impl fmt::Display for BotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for BotError {}

pub type BotResult<T> = Result<T, BotError>;
