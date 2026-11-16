//! Persistence for everything auth-related, on the app's SQLite database.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use serde::Serialize;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;
use ytp_core::{Error, Result};

use crate::permission::parse_lenient;
use crate::role::{self, Quota, Role, ADMIN, BUILTIN_ROLES, DEFAULT_QUOTA};
use crate::{now, password, rfc3339, secret, Permission};

pub const SESSION_TTL_SECS: i64 = 30 * 24 * 3600;
const TOUCH_INTERVAL_SECS: i64 = 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub roles: Vec<String>,
    pub disabled: bool,
    pub created_at: String,
}

impl User {
    pub fn is_admin(&self) -> bool {
        self.roles.iter().any(|r| r == ADMIN)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Token {
    pub id: i64,
    pub name: String,
    pub permissions: Vec<Permission>,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub last_used_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    pub id: i64,
    pub at: String,
    pub actor: Option<String>,
    pub action: String,
    pub target: Option<String>,
    pub detail: Option<serde_json::Value>,
    pub ip: Option<String>,
}

/// How a request proved who it is.
#[derive(Debug, Clone)]
pub enum Credential {
    Session { id_hash: String, csrf: String },
    Token { id: i64 },
}

/// An authenticated caller with its effective permissions.
#[derive(Debug, Clone)]
pub struct Principal {
    pub user: User,
    pub permissions: BTreeSet<Permission>,
    pub quota: Quota,
    pub credential: Credential,
}

impl Principal {
    pub fn has(&self, p: Permission) -> bool {
        self.permissions.contains(&p)
    }

    pub fn require(&self, p: Permission) -> Result<()> {
        if self.has(p) {
            Ok(())
        } else {
            Err(Error::forbidden(format!("missing permission {p}")))
        }
    }

    pub fn csrf(&self) -> Option<&str> {
        match &self.credential {
            Credential::Session { csrf, .. } => Some(csrf),
            Credential::Token { .. } => None,
        }
    }

    pub fn session_hash(&self) -> Option<&str> {
        match &self.credential {
            Credential::Session { id_hash, .. } => Some(id_hash),
            Credential::Token { .. } => None,
        }
    }

    pub fn permission_list(&self) -> Vec<Permission> {
        self.permissions.iter().copied().collect()
    }
}

pub struct NewSession {
    /// Cookie value. Only its hash is stored.
    pub secret: String,
    pub csrf: String,
    pub expires_at: i64,
}

pub fn validate_username(name: &str) -> Result<()> {
    let ok = (1..=32).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(
            "username must be 1-32 characters of letters, digits, '_', '-' or '.'",
        ))
    }
}

pub fn validate_role_name(name: &str) -> Result<()> {
    let ok = (1..=32).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(
            "role name must be 1-32 characters of lowercase letters, digits, '_' or '-'",
        ))
    }
}

type UserRow = (i64, String, bool, i64);
type TokenRow = (i64, String, String, i64, Option<i64>, Option<i64>);
/// id, user_id, permissions, expires_at, last_used_at
type TokenAuthRow = (i64, i64, String, Option<i64>, Option<i64>);
type AuditRow = (
    i64,
    i64,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn token_from_row((id, name, perms, created, expires, used): TokenRow) -> Token {
    let mut permissions = parse_lenient(&perms);
    permissions.sort();
    Token {
        id,
        name,
        permissions,
        created_at: rfc3339(created),
        expires_at: expires.map(rfc3339),
        last_used_at: used.map(rfc3339),
    }
}

fn role_from_row(name: String, builtin: bool, perms: &str, quota: &str) -> Role {
    let mut permissions = if name == ADMIN {
        Permission::ALL.to_vec()
    } else {
        parse_lenient(perms)
    };
    permissions.sort();
    permissions.dedup();
    let quota = serde_json::from_str(quota).unwrap_or(DEFAULT_QUOTA);
    Role {
        name,
        builtin,
        permissions,
        quota,
    }
}

fn perms_json(perms: &[Permission]) -> String {
    let mut v: Vec<&str> = perms.iter().map(|p| p.as_str()).collect();
    v.sort();
    v.dedup();
    serde_json::to_string(&v).unwrap()
}

#[derive(Clone)]
pub struct AuthStore {
    pool: SqlitePool,
}

impl AuthStore {
    /// Open (creating if needed) the app database and bring it up to date.
    pub async fn open(path: &Path) -> Result<Self> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(opts)
            .await?;
        Self::from_pool(pool).await
    }

    pub async fn from_pool(pool: SqlitePool) -> Result<Self> {
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(Error::internal)?;
        let store = Self { pool };
        store.seed_roles().await?;
        Ok(store)
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    async fn seed_roles(&self) -> Result<()> {
        for r in BUILTIN_ROLES {
            // Keep any edits an admin made to a built-in, but ADD the permissions
            // a later version put in the template: otherwise a new permission
            // never reaches an existing install (the admin role is recomputed on
            // every read, the other built-ins are stored once). Additive only -
            // a permission an admin removed from a built-in comes back; a role
            // that needs a smaller set should be a custom role.
            let existing: Option<String> = sqlx::query_scalar("SELECT permissions FROM roles WHERE name = ?")
                .bind(r.name)
                .fetch_optional(&self.pool)
                .await?;
            let merged = match existing {
                None => perms_json(r.permissions),
                Some(json) => {
                    let mut have: Vec<Permission> = serde_json::from_str::<Vec<String>>(&json)
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|s| s.parse::<Permission>().ok())
                        .collect();
                    let before = have.len();
                    for p in r.permissions {
                        if !have.contains(p) {
                            have.push(*p);
                        }
                    }
                    if have.len() == before {
                        // Nothing new: still make sure the row is marked built-in.
                        sqlx::query("UPDATE roles SET builtin = 1 WHERE name = ?")
                            .bind(r.name)
                            .execute(&self.pool)
                            .await?;
                        continue;
                    }
                    perms_json(&have)
                }
            };
            sqlx::query(
                "INSERT INTO roles (name, builtin, permissions, quota) VALUES (?, 1, ?, ?)
                 ON CONFLICT(name) DO UPDATE SET builtin = 1, permissions = excluded.permissions",
            )
            .bind(r.name)
            .bind(merged)
            .bind(serde_json::to_string(&r.quota)?)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    // ── users ────────────────────────────────────────────────────────────

    pub async fn user_count(&self) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT count(*) FROM users")
            .fetch_one(&self.pool)
            .await?)
    }

    async fn roles_of(&self, user_id: i64) -> Result<Vec<String>> {
        Ok(
            sqlx::query_scalar("SELECT role FROM user_roles WHERE user_id = ? ORDER BY role")
                .bind(user_id)
                .fetch_all(&self.pool)
                .await?,
        )
    }

    async fn hydrate(&self, (id, username, disabled, created): UserRow) -> Result<User> {
        Ok(User {
            id,
            username,
            roles: self.roles_of(id).await?,
            disabled,
            created_at: rfc3339(created),
        })
    }

    pub async fn get_user(&self, id: i64) -> Result<User> {
        let row: Option<UserRow> =
            sqlx::query_as("SELECT id, username, disabled, created_at FROM users WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        match row {
            Some(r) => self.hydrate(r).await,
            None => Err(Error::not_found("no such user")),
        }
    }

    pub async fn find_user(&self, username: &str) -> Result<Option<User>> {
        let row: Option<UserRow> = sqlx::query_as(
            "SELECT id, username, disabled, created_at FROM users WHERE username = ?",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(r) => Ok(Some(self.hydrate(r).await?)),
            None => Ok(None),
        }
    }

    pub async fn list_users(&self) -> Result<Vec<User>> {
        let rows: Vec<UserRow> =
            sqlx::query_as("SELECT id, username, disabled, created_at FROM users ORDER BY id")
                .fetch_all(&self.pool)
                .await?;
        let pairs: Vec<(i64, String)> =
            sqlx::query_as("SELECT user_id, role FROM user_roles ORDER BY role")
                .fetch_all(&self.pool)
                .await?;
        let mut roles: BTreeMap<i64, Vec<String>> = BTreeMap::new();
        for (uid, role) in pairs {
            roles.entry(uid).or_default().push(role);
        }
        Ok(rows
            .into_iter()
            .map(|(id, username, disabled, created)| User {
                id,
                username,
                roles: roles.remove(&id).unwrap_or_default(),
                disabled,
                created_at: rfc3339(created),
            })
            .collect())
    }

    async fn check_roles_exist(&self, roles: &[String]) -> Result<Vec<String>> {
        let mut out: Vec<String> = roles.to_vec();
        out.sort();
        out.dedup();
        for r in &out {
            let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM roles WHERE name = ?")
                .bind(r)
                .fetch_optional(&self.pool)
                .await?;
            if exists.is_none() {
                return Err(Error::invalid(format!("unknown role `{r}`")));
            }
        }
        Ok(out)
    }

    pub async fn create_user(
        &self,
        username: &str,
        password: &str,
        roles: &[String],
    ) -> Result<User> {
        validate_username(username)?;
        password::validate(password)?;
        let roles = self.check_roles_exist(roles).await?;
        if self.find_user(username).await?.is_some() {
            return Err(Error::conflict("username is taken"));
        }
        let hash = password::hash(password.to_string()).await?;
        let mut tx = self.pool.begin().await?;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO users (username, password_hash, created_at) VALUES (?, ?, ?) RETURNING id",
        )
        .bind(username)
        .bind(hash)
        .bind(now())
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match Error::from(e) {
            Error::Conflict(_) => Error::conflict("username is taken"),
            other => other,
        })?;
        for r in &roles {
            sqlx::query("INSERT INTO user_roles (user_id, role) VALUES (?, ?)")
                .bind(id)
                .bind(r)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        self.get_user(id).await
    }

    async fn enabled_admins(&self) -> Result<i64> {
        Ok(sqlx::query_scalar(
            "SELECT count(*) FROM users u JOIN user_roles r ON r.user_id = u.id
             WHERE r.role = ? AND u.disabled = 0",
        )
        .bind(ADMIN)
        .fetch_one(&self.pool)
        .await?)
    }

    /// Refuse a change that would leave nobody able to administer the server.
    async fn guard_last_admin(&self, user: &User, still_admin: bool) -> Result<()> {
        if user.is_admin() && !user.disabled && !still_admin && self.enabled_admins().await? <= 1 {
            return Err(Error::conflict("cannot remove the last enabled admin"));
        }
        Ok(())
    }

    pub async fn update_user(
        &self,
        id: i64,
        roles: Option<Vec<String>>,
        disabled: Option<bool>,
        new_password: Option<String>,
    ) -> Result<User> {
        let user = self.get_user(id).await?;
        let roles = match roles {
            Some(r) => Some(self.check_roles_exist(&r).await?),
            None => None,
        };
        let will_be_admin = roles
            .as_ref()
            .map_or(user.is_admin(), |r| r.iter().any(|x| x == ADMIN));
        let will_be_enabled = !disabled.unwrap_or(user.disabled);
        self.guard_last_admin(&user, will_be_admin && will_be_enabled)
            .await?;
        let hash = match &new_password {
            Some(p) => {
                password::validate(p)?;
                Some(password::hash(p.clone()).await?)
            }
            None => None,
        };

        let mut tx = self.pool.begin().await?;
        if let Some(roles) = &roles {
            sqlx::query("DELETE FROM user_roles WHERE user_id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            for r in roles {
                sqlx::query("INSERT INTO user_roles (user_id, role) VALUES (?, ?)")
                    .bind(id)
                    .bind(r)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        if let Some(d) = disabled {
            sqlx::query("UPDATE users SET disabled = ? WHERE id = ?")
                .bind(d)
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        if let Some(h) = &hash {
            sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
                .bind(h)
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        if disabled == Some(true) || hash.is_some() {
            sqlx::query("DELETE FROM sessions WHERE user_id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        self.get_user(id).await
    }

    pub async fn delete_user(&self, id: i64) -> Result<User> {
        let user = self.get_user(id).await?;
        self.guard_last_admin(&user, false).await?;
        sqlx::query("DELETE FROM users WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(user)
    }

    /// Set a password outright and sign the user out everywhere.
    pub async fn set_password(&self, id: i64, new_password: &str) -> Result<()> {
        password::validate(new_password)?;
        let hash = password::hash(new_password.to_string()).await?;
        let done = sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
            .bind(hash)
            .bind(id)
            .execute(&self.pool)
            .await?;
        if done.rows_affected() == 0 {
            return Err(Error::not_found("no such user"));
        }
        self.delete_user_sessions(id, None).await
    }

    /// Change one's own password. Other sessions are signed out; the one
    /// making the change (if any) is kept.
    pub async fn change_password(
        &self,
        id: i64,
        current: &str,
        new_password: &str,
        keep_session_hash: Option<&str>,
    ) -> Result<()> {
        let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await?;
        if !password::verify(current.to_string(), hash).await {
            return Err(Error::forbidden("current password is wrong"));
        }
        password::validate(new_password)?;
        let new_hash = password::hash(new_password.to_string()).await?;
        sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
            .bind(new_hash)
            .bind(id)
            .execute(&self.pool)
            .await?;
        self.delete_user_sessions(id, keep_session_hash).await
    }

    /// Check a username/password. Always performs one Argon2 verification,
    /// whether or not the user exists, so timing does not reveal which.
    pub async fn verify_login(&self, username: &str, pw: &str) -> Result<Option<User>> {
        let row: Option<(i64, String, bool)> =
            sqlx::query_as("SELECT id, password_hash, disabled FROM users WHERE username = ?")
                .bind(username)
                .fetch_optional(&self.pool)
                .await?;
        let (id, hash, disabled) = match row {
            Some(r) => r,
            None => {
                let dummy = password::dummy_hash().to_string();
                password::verify(pw.to_string(), dummy).await;
                return Ok(None);
            }
        };
        let ok = password::verify(pw.to_string(), hash).await;
        if !ok || disabled {
            return Ok(None);
        }
        Ok(Some(self.get_user(id).await?))
    }

    // ── roles ────────────────────────────────────────────────────────────

    /// Effective permissions and quota for a set of role names.
    pub async fn grants(&self, roles: &[String]) -> Result<(BTreeSet<Permission>, Quota)> {
        let mut perms = BTreeSet::new();
        let mut quota = Quota::ZERO;
        for name in roles {
            if let Ok(role) = self.get_role(name).await {
                perms.extend(role.permissions);
                quota = quota.max(role.quota);
            }
        }
        Ok((perms, quota))
    }

    pub async fn list_roles(&self) -> Result<Vec<Role>> {
        let rows: Vec<(String, bool, String, String)> = sqlx::query_as(
            "SELECT name, builtin, permissions, quota FROM roles ORDER BY builtin DESC, name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(n, b, p, q)| role_from_row(n, b, &p, &q))
            .collect())
    }

    pub async fn get_role(&self, name: &str) -> Result<Role> {
        let row: Option<(String, bool, String, String)> =
            sqlx::query_as("SELECT name, builtin, permissions, quota FROM roles WHERE name = ?")
                .bind(name)
                .fetch_optional(&self.pool)
                .await?;
        match row {
            Some((n, b, p, q)) => Ok(role_from_row(n, b, &p, &q)),
            None => Err(Error::not_found("no such role")),
        }
    }

    pub async fn create_role(
        &self,
        name: &str,
        permissions: &[Permission],
        quota: Option<Quota>,
    ) -> Result<Role> {
        validate_role_name(name)?;
        sqlx::query("INSERT INTO roles (name, builtin, permissions, quota) VALUES (?, 0, ?, ?)")
            .bind(name)
            .bind(perms_json(permissions))
            .bind(serde_json::to_string(&quota.unwrap_or(DEFAULT_QUOTA))?)
            .execute(&self.pool)
            .await
            .map_err(|e| match Error::from(e) {
                Error::Conflict(_) => Error::conflict("a role with that name exists"),
                other => other,
            })?;
        self.get_role(name).await
    }

    pub async fn update_role(
        &self,
        name: &str,
        permissions: Option<&[Permission]>,
        quota: Option<Quota>,
    ) -> Result<Role> {
        self.get_role(name).await?;
        if let Some(p) = permissions {
            if name == ADMIN {
                return Err(Error::conflict(
                    "the admin role always has every permission",
                ));
            }
            sqlx::query("UPDATE roles SET permissions = ? WHERE name = ?")
                .bind(perms_json(p))
                .bind(name)
                .execute(&self.pool)
                .await?;
        }
        if let Some(q) = quota {
            sqlx::query("UPDATE roles SET quota = ? WHERE name = ?")
                .bind(serde_json::to_string(&q)?)
                .bind(name)
                .execute(&self.pool)
                .await?;
        }
        self.get_role(name).await
    }

    pub async fn delete_role(&self, name: &str) -> Result<()> {
        let r = self.get_role(name).await?;
        if r.builtin || role::builtin(name).is_some() {
            return Err(Error::conflict("built-in roles cannot be deleted"));
        }
        sqlx::query("DELETE FROM roles WHERE name = ?")
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ── sessions ─────────────────────────────────────────────────────────

    pub async fn create_session(
        &self,
        user_id: i64,
        ip: Option<&str>,
        user_agent: Option<&str>,
    ) -> Result<NewSession> {
        let secret = secret::random_secret();
        let csrf = secret::random_secret();
        let t = now();
        let expires_at = t + SESSION_TTL_SECS;
        sqlx::query(
            "INSERT INTO sessions (id_hash, user_id, csrf, created_at, expires_at, last_seen_at, ip, user_agent)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(secret::hash_secret(&secret))
        .bind(user_id)
        .bind(&csrf)
        .bind(t)
        .bind(expires_at)
        .bind(t)
        .bind(ip)
        .bind(user_agent.map(|u| u.chars().take(256).collect::<String>()))
        .execute(&self.pool)
        .await?;
        Ok(NewSession {
            secret,
            csrf,
            expires_at,
        })
    }

    async fn principal(&self, user_id: i64, credential: Credential) -> Result<Principal> {
        let user = self.get_user(user_id).await?;
        let (permissions, quota) = self.grants(&user.roles).await?;
        Ok(Principal {
            user,
            permissions,
            quota,
            credential,
        })
    }

    /// Resolve a session cookie. `None` for unknown, expired or disabled.
    pub async fn session_principal(&self, cookie: &str) -> Result<Option<Principal>> {
        let id_hash = secret::hash_secret(cookie);
        let row: Option<(i64, String, i64, i64)> = sqlx::query_as(
            "SELECT s.user_id, s.csrf, s.expires_at, s.last_seen_at
             FROM sessions s JOIN users u ON u.id = s.user_id
             WHERE s.id_hash = ? AND u.disabled = 0",
        )
        .bind(&id_hash)
        .fetch_optional(&self.pool)
        .await?;
        let Some((user_id, csrf, expires_at, last_seen)) = row else {
            return Ok(None);
        };
        let t = now();
        if expires_at <= t {
            self.delete_session_hash(&id_hash).await?;
            return Ok(None);
        }
        if t - last_seen > TOUCH_INTERVAL_SECS {
            sqlx::query("UPDATE sessions SET last_seen_at = ? WHERE id_hash = ?")
                .bind(t)
                .bind(&id_hash)
                .execute(&self.pool)
                .await?;
        }
        Ok(Some(
            self.principal(user_id, Credential::Session { id_hash, csrf })
                .await?,
        ))
    }

    pub async fn delete_session(&self, cookie: &str) -> Result<()> {
        self.delete_session_hash(&secret::hash_secret(cookie)).await
    }

    pub async fn delete_session_hash(&self, id_hash: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE id_hash = ?")
            .bind(id_hash)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete_user_sessions(
        &self,
        user_id: i64,
        except_hash: Option<&str>,
    ) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE user_id = ? AND id_hash IS NOT ?")
            .bind(user_id)
            .bind(except_hash)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn purge_expired(&self) -> Result<u64> {
        let t = now();
        let s = sqlx::query("DELETE FROM sessions WHERE expires_at <= ?")
            .bind(t)
            .execute(&self.pool)
            .await?;
        Ok(s.rows_affected())
    }

    // ── API tokens ───────────────────────────────────────────────────────

    /// Mint a token for `owner`. A token can never carry a permission its
    /// creator does not currently hold. Returns the token and its secret,
    /// which is not stored and cannot be shown again.
    pub async fn create_token(
        &self,
        owner: &Principal,
        name: &str,
        permissions: &[Permission],
        expires_days: Option<u32>,
    ) -> Result<(Token, String)> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 64 {
            return Err(Error::invalid("token name must be 1-64 characters"));
        }
        if permissions.is_empty() {
            return Err(Error::invalid("a token needs at least one permission"));
        }
        if let Some(missing) = permissions.iter().find(|p| !owner.has(**p)) {
            return Err(Error::forbidden(format!(
                "cannot grant {missing}: you do not hold it"
            )));
        }
        if let Some(d) = expires_days {
            if !(1..=3650).contains(&d) {
                return Err(Error::invalid("expires_days must be between 1 and 3650"));
            }
        }
        let secret = secret::new_api_token();
        let t = now();
        let expires_at = expires_days.map(|d| t + i64::from(d) * 86_400);
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO api_tokens (user_id, name, secret_hash, permissions, created_at, expires_at)
             VALUES (?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(owner.user.id)
        .bind(name)
        .bind(secret::hash_secret(&secret))
        .bind(perms_json(permissions))
        .bind(t)
        .bind(expires_at)
        .fetch_one(&self.pool)
        .await?;
        let token = self
            .list_tokens(owner.user.id)
            .await?
            .into_iter()
            .find(|tk| tk.id == id)
            .ok_or_else(|| Error::internal("token vanished"))?;
        Ok((token, secret))
    }

    pub async fn list_tokens(&self, user_id: i64) -> Result<Vec<Token>> {
        let rows: Vec<TokenRow> = sqlx::query_as(
            "SELECT id, name, permissions, created_at, expires_at, last_used_at
             FROM api_tokens WHERE user_id = ? ORDER BY id",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(token_from_row).collect())
    }

    pub async fn delete_token(&self, user_id: i64, id: i64) -> Result<()> {
        let r = sqlx::query("DELETE FROM api_tokens WHERE id = ? AND user_id = ?")
            .bind(id)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        if r.rows_affected() == 0 {
            return Err(Error::not_found("no such token"));
        }
        Ok(())
    }

    /// Resolve a bearer secret. Effective permissions are the token's scope
    /// intersected with what its owner holds right now.
    pub async fn token_principal(&self, bearer: &str) -> Result<Option<Principal>> {
        if !bearer.starts_with(secret::TOKEN_PREFIX) {
            return Ok(None);
        }
        let row: Option<TokenAuthRow> = sqlx::query_as(
            "SELECT t.id, t.user_id, t.permissions, t.expires_at, t.last_used_at
             FROM api_tokens t JOIN users u ON u.id = t.user_id
             WHERE t.secret_hash = ? AND u.disabled = 0",
        )
        .bind(secret::hash_secret(bearer))
        .fetch_optional(&self.pool)
        .await?;
        let Some((id, user_id, perms, expires_at, last_used)) = row else {
            return Ok(None);
        };
        let t = now();
        if expires_at.is_some_and(|e| e <= t) {
            return Ok(None);
        }
        if last_used.is_none_or(|u| t - u > TOUCH_INTERVAL_SECS) {
            sqlx::query("UPDATE api_tokens SET last_used_at = ? WHERE id = ?")
                .bind(t)
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        let mut p = self.principal(user_id, Credential::Token { id }).await?;
        let scope: BTreeSet<Permission> = parse_lenient(&perms).into_iter().collect();
        p.permissions = p.permissions.intersection(&scope).copied().collect();
        Ok(Some(p))
    }

    // ── audit ────────────────────────────────────────────────────────────

    /// Append to the audit log. Failures are logged, never propagated: an
    /// audit hiccup must not turn a completed action into an error.
    pub async fn audit(
        &self,
        actor: Option<&str>,
        action: &str,
        target: Option<&str>,
        detail: Option<serde_json::Value>,
        ip: Option<&str>,
    ) {
        let res = sqlx::query(
            "INSERT INTO audit_log (at, actor, action, target, detail, ip) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(now())
        .bind(actor)
        .bind(action)
        .bind(target)
        .bind(detail.map(|d| d.to_string()))
        .bind(ip)
        .execute(&self.pool)
        .await;
        if let Err(e) = res {
            tracing::error!(error = %e, action, "audit write failed");
        }
    }

    /// Newest first. `before` is an entry id: returns entries older than it.
    pub async fn list_audit(&self, limit: i64, before: Option<i64>) -> Result<Vec<AuditEntry>> {
        let rows: Vec<AuditRow> = sqlx::query_as(
            "SELECT id, at, actor, action, target, detail, ip FROM audit_log
             WHERE id < ? ORDER BY id DESC LIMIT ?",
        )
        .bind(before.unwrap_or(i64::MAX))
        .bind(limit.clamp(1, 1000))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, at, actor, action, target, detail, ip)| AuditEntry {
                id,
                at: rfc3339(at),
                actor,
                action,
                target,
                detail: detail.and_then(|d| serde_json::from_str(&d).ok()),
                ip,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn store() -> (tempfile::TempDir, AuthStore) {
        let dir = tempfile::tempdir().unwrap();
        let s = AuthStore::open(&dir.path().join("app.db")).await.unwrap();
        (dir, s)
    }

    #[tokio::test]
    async fn seeding_adds_new_builtin_permissions_without_dropping_edits() {
        let (_d, s) = store().await;
        // An install from before `media.install` existed, plus an admin's own
        // addition to the editor role.
        sqlx::query("UPDATE roles SET permissions = ? WHERE name = 'editor'")
            .bind(r#"["generate.ytp","settings.manage"]"#)
            .execute(s.pool())
            .await
            .unwrap();
        // A restart re-seeds.
        s.seed_roles().await.unwrap();

        let editor = s.get_role("editor").await.unwrap();
        assert!(
            editor.permissions.contains(&Permission::MediaInstall),
            "a new template permission must reach an existing install: {:?}",
            editor.permissions
        );
        assert!(
            editor.permissions.contains(&Permission::SettingsManage),
            "the admin's own grant must survive: {:?}",
            editor.permissions
        );
        assert!(editor.permissions.contains(&Permission::GenerateYtp));

        // Seeding again changes nothing (the merge is idempotent).
        let before = editor.permissions.len();
        s.seed_roles().await.unwrap();
        assert_eq!(s.get_role("editor").await.unwrap().permissions.len(), before);
    }

    #[tokio::test]
    async fn users_sessions_tokens() {
        let (_d, s) = store().await;
        assert_eq!(s.user_count().await.unwrap(), 0);
        let admin = s
            .create_user("root", "password123", &["admin".into()])
            .await
            .unwrap();
        assert!(matches!(
            s.create_user("ROOT", "password123", &[]).await,
            Err(Error::Conflict(_))
        ));
        assert!(s.verify_login("root", "nope").await.unwrap().is_none());
        assert!(s.verify_login("ghost", "nope").await.unwrap().is_none());
        assert!(s
            .verify_login("root", "password123")
            .await
            .unwrap()
            .is_some());

        // Last admin cannot be demoted, disabled or deleted.
        assert!(s
            .update_user(admin.id, Some(vec![]), None, None)
            .await
            .is_err());
        assert!(s
            .update_user(admin.id, None, Some(true), None)
            .await
            .is_err());
        assert!(s.delete_user(admin.id).await.is_err());

        let sess = s.create_session(admin.id, None, None).await.unwrap();
        let p = s.session_principal(&sess.secret).await.unwrap().unwrap();
        assert_eq!(p.permissions.len(), Permission::ALL.len());
        assert_eq!(p.csrf(), Some(sess.csrf.as_str()));

        let (tok, secret) = s
            .create_token(&p, "ci", &[Permission::CorpusRead], Some(1))
            .await
            .unwrap();
        let tp = s.token_principal(&secret).await.unwrap().unwrap();
        assert_eq!(tp.permission_list(), vec![Permission::CorpusRead]);
        s.delete_token(admin.id, tok.id).await.unwrap();
        assert!(s.token_principal(&secret).await.unwrap().is_none());

        s.set_password(admin.id, "another-pass").await.unwrap();
        assert!(s.session_principal(&sess.secret).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn roles() {
        let (_d, s) = store().await;
        let names: Vec<_> = s
            .list_roles()
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect();
        for b in ["admin", "editor", "creator", "guest"] {
            assert!(names.contains(&b.to_string()));
        }
        assert!(matches!(
            s.delete_role("guest").await,
            Err(Error::Conflict(_))
        ));
        let r = s
            .create_role("viewer", &[Permission::CorpusRead], None)
            .await
            .unwrap();
        assert!(!r.builtin);
        s.delete_role("viewer").await.unwrap();
        assert!(matches!(
            s.update_role("admin", Some(&[]), None).await,
            Err(Error::Conflict(_))
        ));
    }
}
