-- Timeline projects (the Studio's documents) and who may open them.
-- `data` is the project JSON (ytp-core timeline model); the other columns
-- are what lists and permission checks need without parsing it.
CREATE TABLE projects (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    data TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'sequence',
    duration REAL NOT NULL DEFAULT 0,
    clips INTEGER NOT NULL DEFAULT 0,
    revision INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX idx_projects_owner ON projects(owner_id);

CREATE TABLE project_shares (
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    access TEXT NOT NULL CHECK (access IN ('read', 'write')),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (project_id, user_id)
);
CREATE INDEX idx_project_shares_user ON project_shares(user_id);
