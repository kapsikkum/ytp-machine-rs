use serde::{Deserialize, Serialize};

use crate::Permission;
use Permission::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quota {
    pub max_concurrent_jobs: u32,
    pub max_render_seconds: u32,
    pub max_height: u32,
    pub requests_per_minute: u32,
}

impl Quota {
    /// The most generous of two quotas: a user with several roles gets the
    /// best limit each role offers.
    pub fn max(self, o: Quota) -> Quota {
        Quota {
            max_concurrent_jobs: self.max_concurrent_jobs.max(o.max_concurrent_jobs),
            max_render_seconds: self.max_render_seconds.max(o.max_render_seconds),
            max_height: self.max_height.max(o.max_height),
            requests_per_minute: self.requests_per_minute.max(o.requests_per_minute),
        }
    }

    pub const ZERO: Quota = Quota {
        max_concurrent_jobs: 0,
        max_render_seconds: 0,
        max_height: 0,
        requests_per_minute: 0,
    };
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Role {
    pub name: String,
    pub builtin: bool,
    pub permissions: Vec<Permission>,
    pub quota: Quota,
}

pub const ADMIN: &str = "admin";
pub const EDITOR: &str = "editor";
pub const CREATOR: &str = "creator";
pub const GUEST: &str = "guest";

pub struct BuiltinRole {
    pub name: &'static str,
    pub permissions: &'static [Permission],
    pub quota: Quota,
}

const EDITOR_PERMS: &[Permission] = &[
    GenerateYtp,
    GenerateYtpmv,
    StudioUse,
    ProjectRead,
    ProjectWrite,
    ProjectShare,
    ExportRender,
    ExportHq,
    CorpusRead,
    CorpusEdit,
    CorpusInstall,
    JobsViewAll,
    TokensCreate,
];

const CREATOR_PERMS: &[Permission] = &[
    GenerateYtp,
    GenerateYtpmv,
    StudioUse,
    ProjectRead,
    ProjectWrite,
    ExportRender,
    CorpusRead,
    TokensCreate,
];

const GUEST_PERMS: &[Permission] = &[GenerateYtp, ProjectRead, CorpusRead];

/// Built-in roles, seeded on startup. `admin` always holds every permission,
/// including ones added by later versions.
pub const BUILTIN_ROLES: &[BuiltinRole] = &[
    BuiltinRole {
        name: ADMIN,
        permissions: Permission::ALL,
        quota: Quota {
            max_concurrent_jobs: 8,
            max_render_seconds: 3600,
            max_height: 2160,
            requests_per_minute: 600,
        },
    },
    BuiltinRole {
        name: EDITOR,
        permissions: EDITOR_PERMS,
        quota: Quota {
            max_concurrent_jobs: 4,
            max_render_seconds: 1200,
            max_height: 1080,
            requests_per_minute: 240,
        },
    },
    BuiltinRole {
        name: CREATOR,
        permissions: CREATOR_PERMS,
        quota: Quota {
            max_concurrent_jobs: 2,
            max_render_seconds: 300,
            max_height: 720,
            requests_per_minute: 120,
        },
    },
    BuiltinRole {
        name: GUEST,
        permissions: GUEST_PERMS,
        quota: Quota {
            max_concurrent_jobs: 1,
            max_render_seconds: 60,
            max_height: 480,
            requests_per_minute: 30,
        },
    },
];

pub fn builtin(name: &str) -> Option<&'static BuiltinRole> {
    BUILTIN_ROLES.iter().find(|r| r.name == name)
}

/// Quota given to a custom role created without one.
pub const DEFAULT_QUOTA: Quota = BUILTIN_ROLES[2].quota;
