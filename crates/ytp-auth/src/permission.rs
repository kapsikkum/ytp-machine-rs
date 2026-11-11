use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

macro_rules! permissions {
    ($($variant:ident => $id:literal),* $(,)?) => {
        /// A capability. Serialised as the string ids from `docs/api.md`.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Permission {
            $($variant),*
        }

        impl Permission {
            pub const ALL: &'static [Permission] = &[$(Permission::$variant),*];

            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Permission::$variant => $id),*
                }
            }
        }

        impl FromStr for Permission {
            type Err = UnknownPermission;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($id => Ok(Permission::$variant),)*
                    _ => Err(UnknownPermission(s.to_string())),
                }
            }
        }
    };
}

permissions! {
    GenerateYtp => "generate.ytp",
    GenerateYtpmv => "generate.ytpmv",
    StudioUse => "studio.use",
    ProjectRead => "project.read",
    ProjectWrite => "project.write",
    ProjectShare => "project.share",
    ExportRender => "export.render",
    ExportHq => "export.hq",
    CorpusRead => "corpus.read",
    CorpusEdit => "corpus.edit",
    CorpusInstall => "corpus.install",
    CorpusDelete => "corpus.delete",
    JobsViewAll => "jobs.view_all",
    JobsCancelAny => "jobs.cancel_any",
    UsersManage => "users.manage",
    RolesManage => "roles.manage",
    TokensCreate => "tokens.create",
    SettingsManage => "settings.manage",
    AuditView => "audit.view",
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownPermission(pub String);

impl fmt::Display for UnknownPermission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown permission `{}`", self.0)
    }
}

impl std::error::Error for UnknownPermission {}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for Permission {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Permission {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Parse a stored permission list, silently dropping ids this build does not
/// know (a newer build may have written them).
pub fn parse_lenient(json: &str) -> Vec<Permission> {
    serde_json::from_str::<Vec<String>>(json)
        .unwrap_or_default()
        .iter()
        .filter_map(|s| s.parse().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_roundtrip() {
        assert_eq!(Permission::ALL.len(), 19);
        for p in Permission::ALL {
            assert_eq!(p.as_str().parse::<Permission>().unwrap(), *p);
            let json = serde_json::to_string(p).unwrap();
            assert_eq!(json, format!("\"{}\"", p.as_str()));
            assert_eq!(serde_json::from_str::<Permission>(&json).unwrap(), *p);
        }
        assert!(serde_json::from_str::<Permission>("\"nope\"").is_err());
    }
}
