//! Stored timeline projects and who may open them. The document itself is an
//! opaque JSON string here (its model lives in `ytp-core`); this module keeps
//! the rows, the owner, the shares and a revision counter for editing.

use serde::Serialize;
use ytp_core::time::now;
use ytp_core::{Error, Result};

use crate::store::AuthStore;

/// Most projects one user owns.
pub const MAX_PROJECTS_PER_USER: i64 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    Read,
    Write,
    Owner,
}

impl Access {
    pub fn can_write(self) -> bool {
        self >= Access::Write
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Access::Read => "read",
            Access::Write => "write",
            Access::Owner => "owner",
        }
    }
}

/// What a project list shows.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectMeta {
    pub id: i64,
    pub owner: i64,
    pub owner_name: String,
    pub name: String,
    pub kind: String,
    pub duration: f64,
    pub clips: i64,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
    /// The caller's access: `owner`, `write` or `read`.
    pub access: Access,
}

#[derive(Debug, Clone)]
pub struct StoredProject {
    pub meta: ProjectMeta,
    pub data: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Share {
    pub user_id: i64,
    pub username: String,
    pub access: Access,
}

type MetaRow = (i64, i64, String, String, String, f64, i64, i64, i64, i64, Option<String>);

fn meta_of(r: MetaRow, viewer: i64) -> ProjectMeta {
    let (id, owner, owner_name, name, kind, duration, clips, revision, created, updated, share) = r;
    let access = if owner == viewer {
        Access::Owner
    } else if share.as_deref() == Some("write") {
        Access::Write
    } else {
        Access::Read
    };
    ProjectMeta {
        id,
        owner,
        owner_name,
        name,
        kind,
        duration,
        clips,
        revision,
        created_at: ytp_core::time::rfc3339(created),
        updated_at: ytp_core::time::rfc3339(updated),
        access,
    }
}

const SELECT: &str = "SELECT p.id, p.owner_id, u.username, p.name, p.kind, p.duration, p.clips, p.revision,
        p.created_at, p.updated_at, s.access
    FROM projects p
    JOIN users u ON u.id = p.owner_id
    LEFT JOIN project_shares s ON s.project_id = p.id AND s.user_id = ?";

impl AuthStore {
    /// Store a new project for `owner`; its id.
    pub async fn project_create(&self, owner: i64, name: &str, data: &str, kind: &str, duration: f64, clips: i64) -> Result<i64> {
        let have: i64 = sqlx::query_scalar("SELECT count(*) FROM projects WHERE owner_id = ?")
            .bind(owner)
            .fetch_one(self.pool())
            .await?;
        if have >= MAX_PROJECTS_PER_USER {
            return Err(Error::conflict(format!("you already have {MAX_PROJECTS_PER_USER} projects; delete some first")));
        }
        let t = now();
        let id = sqlx::query_scalar(
            "INSERT INTO projects (owner_id, name, data, kind, duration, clips, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(owner)
        .bind(name)
        .bind(data)
        .bind(kind)
        .bind(duration)
        .bind(clips)
        .bind(t)
        .bind(t)
        .fetch_one(self.pool())
        .await?;
        Ok(id)
    }

    /// Projects `user` owns or has been given, newest first.
    pub async fn project_list(&self, user: i64) -> Result<Vec<ProjectMeta>> {
        let rows: Vec<MetaRow> = sqlx::query_as(&format!(
            "{SELECT} WHERE p.owner_id = ?1 OR s.user_id = ?1 ORDER BY p.updated_at DESC, p.id DESC"
        ))
        .bind(user)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.into_iter().map(|r| meta_of(r, user)).collect())
    }

    /// The project, if `user` may see it (owner or shared with them).
    pub async fn project_get(&self, id: i64, user: i64) -> Result<Option<StoredProject>> {
        let row: Option<MetaRow> = sqlx::query_as(&format!("{SELECT} WHERE p.id = ?2 AND (p.owner_id = ?1 OR s.user_id = ?1)"))
            .bind(user)
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        let Some(row) = row else { return Ok(None) };
        let data: String = sqlx::query_scalar("SELECT data FROM projects WHERE id = ?")
            .bind(id)
            .fetch_one(self.pool())
            .await?;
        Ok(Some(StoredProject { meta: meta_of(row, user), data }))
    }

    /// Replace a project's document and name. With `if_revision`, only when
    /// the stored revision is that one (a lost update is a conflict). The new revision.
    #[allow(clippy::too_many_arguments)]
    pub async fn project_update(
        &self,
        id: i64,
        name: &str,
        data: &str,
        kind: &str,
        duration: f64,
        clips: i64,
        if_revision: Option<i64>,
    ) -> Result<i64> {
        let r = sqlx::query(
            "UPDATE projects SET name = ?, data = ?, kind = ?, duration = ?, clips = ?,
                 revision = revision + 1, updated_at = ?
             WHERE id = ? AND (? IS NULL OR revision = ?)",
        )
        .bind(name)
        .bind(data)
        .bind(kind)
        .bind(duration)
        .bind(clips)
        .bind(now())
        .bind(id)
        .bind(if_revision)
        .bind(if_revision)
        .execute(self.pool())
        .await?;
        if r.rows_affected() == 0 {
            let exists: i64 = sqlx::query_scalar("SELECT count(*) FROM projects WHERE id = ?")
                .bind(id)
                .fetch_one(self.pool())
                .await?;
            return Err(if exists == 0 {
                Error::not_found("no such project")
            } else {
                Error::conflict("the project was changed by someone else; reload it")
            });
        }
        Ok(sqlx::query_scalar("SELECT revision FROM projects WHERE id = ?").bind(id).fetch_one(self.pool()).await?)
    }

    pub async fn project_delete(&self, id: i64) -> Result<()> {
        let r = sqlx::query("DELETE FROM projects WHERE id = ?").bind(id).execute(self.pool()).await?;
        if r.rows_affected() == 0 {
            return Err(Error::not_found("no such project"));
        }
        Ok(())
    }

    pub async fn project_shares(&self, id: i64) -> Result<Vec<Share>> {
        let rows: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT s.user_id, u.username, s.access FROM project_shares s
             JOIN users u ON u.id = s.user_id WHERE s.project_id = ? ORDER BY u.username",
        )
        .bind(id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|(user_id, username, a)| Share { user_id, username, access: if a == "write" { Access::Write } else { Access::Read } })
            .collect())
    }

    /// Give `user` access to the project (changing it if they already have some).
    pub async fn project_share(&self, id: i64, user: i64, access: Access) -> Result<()> {
        if access == Access::Owner {
            return Err(Error::invalid("a share is read or write"));
        }
        sqlx::query(
            "INSERT INTO project_shares (project_id, user_id, access, created_at) VALUES (?, ?, ?, ?)
             ON CONFLICT(project_id, user_id) DO UPDATE SET access = excluded.access",
        )
        .bind(id)
        .bind(user)
        .bind(access.as_str())
        .bind(now())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn project_unshare(&self, id: i64, user: i64) -> Result<()> {
        let r = sqlx::query("DELETE FROM project_shares WHERE project_id = ? AND user_id = ?")
            .bind(id)
            .bind(user)
            .execute(self.pool())
            .await?;
        if r.rows_affected() == 0 {
            return Err(Error::not_found("that project is not shared with them"));
        }
        Ok(())
    }

    /// How many stored projects mention each of `assets` as a `media` clip
    /// source, across every owner. The document is opaque here, so the count
    /// is read out of the JSON; an id no project names is absent.
    pub async fn project_asset_counts(
        &self,
        assets: &[String],
    ) -> Result<std::collections::HashMap<String, i64>> {
        let mut counts = std::collections::HashMap::new();
        if assets.is_empty() {
            return Ok(counts);
        }
        let wanted: std::collections::HashSet<&str> = assets.iter().map(String::as_str).collect();
        let rows: Vec<String> = sqlx::query_scalar("SELECT data FROM projects")
            .fetch_all(self.pool())
            .await?;
        for data in rows {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) else {
                continue;
            };
            for track in v["tracks"].as_array().into_iter().flatten() {
                for clip in track["clips"].as_array().into_iter().flatten() {
                    let source = &clip["source"];
                    if source["type"] != "media" {
                        continue;
                    }
                    if let Some(id) = source["asset"].as_str().filter(|id| wanted.contains(id)) {
                        *counts.entry(id.to_string()).or_insert(0) += 1;
                    }
                }
            }
        }
        Ok(counts)
    }
}
