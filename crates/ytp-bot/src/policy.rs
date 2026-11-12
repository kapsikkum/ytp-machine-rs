//! Who may use a bot, and which app role they speak as.
//!
//! Allow-lists are per platform. An allowed chat user is mapped to one or more
//! app roles from `ytp-auth`; a Discord member's role ids can pick specific
//! roles, and everyone else who is allowed gets the configured default role
//! (`guest` unless set otherwise).

use std::collections::{BTreeSet, HashMap, HashSet};

/// Matrix access: room ids the bot may answer in, and user ids it may answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MatrixAccess {
    pub rooms: HashSet<String>,
    pub users: HashSet<String>,
}

impl MatrixAccess {
    pub fn user_allowed(&self, user: &str) -> bool {
        if self.users.is_empty() {
            return true;
        }
        let server = user.split_once(':').map(|(_, s)| s).unwrap_or("");
        self.users.contains(user) || (!server.is_empty() && self.users.contains(&format!("*:{server}")))
    }

    pub fn room_allowed(&self, room: &str) -> bool {
        self.rooms.is_empty() || self.rooms.contains(room)
    }

    pub fn allowed(&self, user: &str, room: &str) -> bool {
        // With both lists set, either is enough: an allowed room is open to
        // everyone in it, and an allowed person may use the bot anywhere.
        if !self.rooms.is_empty() && !self.users.is_empty() {
            return self.rooms.contains(room) || self.user_allowed(user);
        }
        self.room_allowed(room) && self.user_allowed(user)
    }
}

/// Discord access: guilds, channels and users, plus role-id → app-role.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscordAccess {
    pub guilds: HashSet<String>,
    pub channels: HashSet<String>,
    pub users: HashSet<String>,
    /// Discord role id → app role name.
    pub roles: HashMap<String, String>,
}

impl DiscordAccess {
    /// With any list set, a match in any of them is enough. With none set the
    /// bot answers anyone who can see it.
    pub fn allowed(&self, user: &str, guild: Option<&str>, channel: &str) -> bool {
        if self.guilds.is_empty() && self.channels.is_empty() && self.users.is_empty() {
            return true;
        }
        self.users.contains(user)
            || self.guilds.contains(guild.unwrap_or(""))
            || self.channels.contains(channel)
    }

    /// The app roles a member's Discord role ids map to, or the default.
    pub fn roles_for(&self, role_ids: &[String], default_role: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        for id in role_ids {
            if let Some(role) = self.roles.get(id) {
                if seen.insert(role.clone()) {
                    out.push(role.clone());
                }
            }
        }
        if out.is_empty() && !default_role.is_empty() {
            out.push(default_role.to_string());
        }
        out
    }
}

/// Everything the core needs to decide who someone is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BotPolicy {
    pub default_role: String,
    pub matrix: MatrixAccess,
    pub discord: DiscordAccess,
}

impl BotPolicy {
    pub fn matrix_allowed(&self, user: &str, room: &str) -> bool {
        self.matrix.allowed(user, room)
    }

    pub fn discord_allowed(&self, user: &str, guild: Option<&str>, channel: &str) -> bool {
        self.discord.allowed(user, guild, channel)
    }

    pub fn discord_roles(&self, role_ids: &[String]) -> Vec<String> {
        self.discord.roles_for(role_ids, &self.default_role)
    }

    pub fn matrix_roles(&self) -> Vec<String> {
        if self.default_role.is_empty() {
            Vec::new()
        } else {
            vec![self.default_role.clone()]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn matrix_lists() {
        let mut a = MatrixAccess::default();
        assert!(a.allowed("@me:example.org", "!room:example.org"));

        a.rooms = set(&["!allowed:example.org"]);
        assert!(a.allowed("@anyone:example.org", "!allowed:example.org"));
        assert!(!a.allowed("@anyone:example.org", "!other:example.org"));

        a.users = set(&["@friend:example.org", "*:trusted.example"]);
        // either is enough when both are set
        assert!(a.allowed("@friend:example.org", "!other:example.org"));
        assert!(a.allowed("@someone:trusted.example", "!other:example.org"));
        assert!(a.allowed("@anyone:example.org", "!allowed:example.org"));
        assert!(!a.allowed("@stranger:elsewhere", "!other:example.org"));
    }

    #[test]
    fn discord_lists_and_roles() {
        let mut a = DiscordAccess::default();
        assert!(a.allowed("1", Some("9"), "7"));

        a.guilds = set(&["9"]);
        assert!(a.allowed("1", Some("9"), "7"));
        assert!(!a.allowed("1", Some("8"), "7"));

        a.channels = set(&["7"]);
        assert!(a.allowed("1", None, "7"));
        assert!(!a.allowed("1", None, "6"));

        a.users = set(&["1"]);
        assert!(a.allowed("1", None, "6"));
        assert!(!a.allowed("2", None, "6"));

        a.roles.insert("42".into(), "creator".into());
        a.roles.insert("43".into(), "editor".into());
        assert_eq!(a.roles_for(&["42".into(), "43".into()], "guest"), vec!["creator", "editor"]);
        assert_eq!(a.roles_for(&["99".into()], "guest"), vec!["guest"]);
        assert!(a.roles_for(&["99".into()], "").is_empty());
    }
}
