-- Users, roles, sessions, API tokens and the audit log.
-- Timestamps are unix seconds (UTC); the API renders them as RFC 3339.

CREATE TABLE users (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    username      TEXT    NOT NULL UNIQUE COLLATE NOCASE,
    password_hash TEXT    NOT NULL,
    disabled      INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL
);

CREATE TABLE roles (
    name        TEXT    PRIMARY KEY,
    builtin     INTEGER NOT NULL DEFAULT 0,
    permissions TEXT    NOT NULL,  -- JSON array of permission ids
    quota       TEXT    NOT NULL   -- JSON Quota object
);

CREATE TABLE user_roles (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role    TEXT    NOT NULL REFERENCES roles(name) ON DELETE CASCADE,
    PRIMARY KEY (user_id, role)
);

-- id_hash is SHA-256 of the cookie value; the cookie itself is never stored.
CREATE TABLE sessions (
    id_hash      TEXT    PRIMARY KEY,
    user_id      INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    csrf         TEXT    NOT NULL,
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    ip           TEXT,
    user_agent   TEXT
);
CREATE INDEX idx_sessions_user ON sessions(user_id);
CREATE INDEX idx_sessions_expiry ON sessions(expires_at);

CREATE TABLE api_tokens (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id      INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name         TEXT    NOT NULL,
    secret_hash  TEXT    NOT NULL UNIQUE,
    permissions  TEXT    NOT NULL,
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER,
    last_used_at INTEGER
);
CREATE INDEX idx_tokens_user ON api_tokens(user_id);

-- actor is a username, not a foreign key: entries outlive the users they name.
CREATE TABLE audit_log (
    id     INTEGER PRIMARY KEY AUTOINCREMENT,
    at     INTEGER NOT NULL,
    actor  TEXT,
    action TEXT    NOT NULL,
    target TEXT,
    detail TEXT,
    ip     TEXT
);
