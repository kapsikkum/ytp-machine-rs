//! Bot configuration, all from the environment (and, on the server's side,
//! from `--matrix-bot` / `--discord-bot`).
//!
//! ```text
//! YTP_BOT_USER            app account the bot speaks as (default "bot")
//! YTP_BOT_DEFAULT_ROLE    role for allowed users with no mapping (default "guest")
//! YTP_BOT_PREFIX          command prefix when a platform names none (default "!ytp")
//! YTP_BOT_MV_MAX_SECONDS  longest song a chat command may make (default 120)
//!
//! YTP_MATRIX_BOT          1/0 to force on/off (otherwise credentials decide)
//! YTP_MATRIX_HOMESERVER   https://matrix.example.org
//! YTP_MATRIX_USER         @ytp:example.org
//! YTP_MATRIX_PASSWORD     ...or
//! YTP_MATRIX_TOKEN        an access token
//! YTP_MATRIX_ROOMS        comma-separated room ids the bot may answer in
//! YTP_MATRIX_USERS        comma-separated user ids (the @ may be left off;
//!                         *:example.org allows a whole server)
//! YTP_MATRIX_PREFIX       default !ytp
//! YTP_MATRIX_DEVICE_NAME  default "ytp-machine"
//!
//! YTP_DISCORD_BOT        1/0 to force on/off (otherwise the token decides)
//! YTP_DISCORD_TOKEN       bot token
//! YTP_DISCORD_GUILDS      comma-separated guild (server) ids
//! YTP_DISCORD_CHANNELS    comma-separated channel ids
//! YTP_DISCORD_USERS       comma-separated user ids
//! YTP_DISCORD_ROLES       role-id=app-role pairs, comma separated
//! YTP_DISCORD_DEFAULT_ROLE  overrides YTP_BOT_DEFAULT_ROLE
//! YTP_DISCORD_PREFIX      default !ytp
//! YTP_DISCORD_PREFIX_COMMANDS  also answer "!ytp ..." messages (needs the
//!                         Message Content intent)
//! ```

use std::collections::{HashMap, HashSet};

use crate::commands::DEFAULT_PREFIX;
use crate::policy::{BotPolicy, DiscordAccess, MatrixAccess};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatrixSettings {
    pub homeserver: String,
    pub user: String,
    pub password: String,
    pub token: String,
    pub device_name: String,
    pub prefix: String,
    pub rooms: HashSet<String>,
    pub users: HashSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordSettings {
    pub token: String,
    pub prefix: String,
    pub prefix_commands: bool,
    pub guilds: HashSet<String>,
    pub channels: HashSet<String>,
    pub users: HashSet<String>,
    pub roles: HashMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    pub service_user: String,
    pub default_role: String,
    pub prefix: String,
    pub mv_max_seconds: u64,
    pub matrix: Option<MatrixSettings>,
    pub discord: Option<DiscordSettings>,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

pub fn truthy(v: &str) -> bool {
    matches!(
        v.trim().to_lowercase().as_str(),
        "1" | "yes" | "on" | "true" | "y"
    )
}

fn csv(name: &str) -> HashSet<String> {
    env(name)
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// A user list, forgiving the missing `@` everyone leaves off. `*:server` is a
/// wildcard and is left alone.
fn matrix_users(name: &str) -> HashSet<String> {
    csv(name)
        .into_iter()
        .map(|u| {
            if u.starts_with('@') || u.starts_with('*') {
                u
            } else {
                format!("@{u}")
            }
        })
        .collect()
}

fn flag_or_credentials(var: &str, has_credentials: bool) -> bool {
    match env(var) {
        Some(v) => truthy(&v),
        None => has_credentials,
    }
}

impl Settings {
    pub fn from_env() -> Self {
        let prefix = env("YTP_BOT_PREFIX").unwrap_or_else(|| DEFAULT_PREFIX.to_string());
        let default_role = env("YTP_BOT_DEFAULT_ROLE").unwrap_or_else(|| "guest".into());

        let homeserver = env("YTP_MATRIX_HOMESERVER").unwrap_or_default();
        let user = env("YTP_MATRIX_USER").unwrap_or_default();
        let password = env("YTP_MATRIX_PASSWORD").unwrap_or_default();
        let token = env("YTP_MATRIX_TOKEN").unwrap_or_default();
        let has_matrix = !homeserver.is_empty() && !user.is_empty() && (!password.is_empty() || !token.is_empty());
        let matrix = if flag_or_credentials("YTP_MATRIX_BOT", has_matrix) && has_matrix {
            Some(MatrixSettings {
                homeserver: homeserver.trim_end_matches('/').to_string(),
                user,
                password,
                token,
                device_name: env("YTP_MATRIX_DEVICE_NAME").unwrap_or_else(|| "ytp-machine".into()),
                prefix: env("YTP_MATRIX_PREFIX").unwrap_or_else(|| prefix.clone()),
                rooms: csv("YTP_MATRIX_ROOMS"),
                users: matrix_users("YTP_MATRIX_USERS"),
            })
        } else {
            None
        };

        let discord_token = env("YTP_DISCORD_TOKEN").unwrap_or_default();
        let discord = if flag_or_credentials("YTP_DISCORD_BOT", !discord_token.is_empty())
            && !discord_token.is_empty()
        {
            Some(DiscordSettings {
                token: discord_token,
                prefix: env("YTP_DISCORD_PREFIX").unwrap_or_else(|| prefix.clone()),
                prefix_commands: env("YTP_DISCORD_PREFIX_COMMANDS")
                    .is_some_and(|v| truthy(&v)),
                guilds: csv("YTP_DISCORD_GUILDS"),
                channels: csv("YTP_DISCORD_CHANNELS"),
                users: csv("YTP_DISCORD_USERS"),
                roles: env("YTP_DISCORD_ROLES")
                    .map(|v| {
                        v.split(',')
                            .filter_map(|pair| pair.split_once('='))
                            .map(|(id, role)| (id.trim().to_string(), role.trim().to_string()))
                            .filter(|(id, role)| !id.is_empty() && !role.is_empty())
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        } else {
            None
        };

        Settings {
            service_user: env("YTP_BOT_USER").unwrap_or_else(|| "bot".into()),
            default_role: env("YTP_DISCORD_DEFAULT_ROLE").unwrap_or(default_role),
            prefix,
            mv_max_seconds: env("YTP_BOT_MV_MAX_SECONDS")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(120)
                .max(1),
            matrix,
            discord,
        }
    }

    pub fn any_enabled(&self) -> bool {
        self.matrix.is_some() || self.discord.is_some()
    }

    pub fn policy(&self) -> BotPolicy {
        BotPolicy {
            default_role: self.default_role.clone(),
            matrix: self.matrix.as_ref().map(|m| MatrixAccess {
                rooms: m.rooms.clone(),
                users: m.users.clone(),
            }).unwrap_or_default(),
            discord: self.discord.as_ref().map(|d| DiscordAccess {
                guilds: d.guilds.clone(),
                channels: d.channels.clone(),
                users: d.users.clone(),
                roles: d.roles.clone(),
            }).unwrap_or_default(),
        }
    }
}
