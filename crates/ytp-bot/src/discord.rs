//! The Discord adapter: slash commands (`/ytp say`, `/ytp mv`, ...) through
//! poise, and optionally the same `!ytp` text commands the Matrix bot has.
//!
//! Every command is turned into the text the core already understands and
//! handed to [`BotCore::handle`], so permissions, rate limits and the job flow
//! are the same as on Matrix. This module owns only what is Discord's: the
//! deferred reply the first status message edits, follow-up messages, the
//! guild's upload limit and fetching an attachment.
//!
//! Slash commands work with no privileged intent. Reading `!ytp` messages, and
//! remembering MIDI files posted in a channel for a later `/ytp mv` with no
//! attachment, need the Message Content intent
//! ([`DiscordSettings::message_content`](crate::settings::DiscordSettings)).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use poise::serenity_prelude as serenity;

use crate::backend::{BotResult, EncodedFile, MidiFile, MidiRef};
use crate::commands::is_midi;
use crate::core::BotCore;
use crate::error::BotError;
use crate::settings::DiscordSettings;
use crate::status::StatusHandle;
use crate::transport::{ChatEvent, ChatTransport, Platform};

/// Matches the web app's MIDI upload limit.
const MAX_MIDI_BYTES: u64 = 2 << 20;
const MESSAGE_LIMIT: usize = 2000;
const BACKOFF_MAX_SECS: u64 = 60;

type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

/// What every command handler can reach.
#[derive(Clone)]
pub struct Data {
    core: Arc<BotCore>,
    settings: Arc<DiscordSettings>,
    status: Arc<StatusHandle>,
    web: reqwest::Client,
}

/// The encode preset for a guild's boost level; a DM or an unknown guild gets
/// the smallest limit.
fn preset_for(tier: Option<serenity::PremiumTier>) -> &'static str {
    match tier {
        Some(serenity::PremiumTier::Tier1) => "discord-25mb",
        Some(serenity::PremiumTier::Tier2 | serenity::PremiumTier::Tier3) => "discord-50mb",
        _ => "discord-10mb",
    }
}

fn guild_tier(cache: &serenity::Cache, guild: Option<serenity::GuildId>) -> Option<serenity::PremiumTier> {
    guild.and_then(|id| cache.guild(id).map(|g| g.premium_tier))
}

/// Split on line breaks (or, failing that, anywhere) into Discord-sized pieces.
fn split_message(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let flush = |current: &mut String, out: &mut Vec<String>| {
        if !current.is_empty() {
            out.push(std::mem::take(current));
        }
    };
    for line in text.split_inclusive('\n') {
        let mut line = line;
        while line.chars().count() > MESSAGE_LIMIT {
            flush(&mut current, &mut out);
            let cut = line
                .char_indices()
                .nth(MESSAGE_LIMIT)
                .map(|(i, _)| i)
                .unwrap_or(line.len());
            out.push(line[..cut].to_string());
            line = &line[cut..];
        }
        if current.chars().count() + line.chars().count() > MESSAGE_LIMIT {
            flush(&mut current, &mut out);
        }
        current.push_str(line);
    }
    flush(&mut current, &mut out);
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

fn no_mentions() -> serenity::CreateAllowedMentions {
    serenity::CreateAllowedMentions::new()
}

/// Is this a URL Discord's own CDN serves attachments from? Anything else is
/// never fetched.
fn is_discord_cdn(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    !host.contains('@')
        && !host.contains(':')
        && (host == "cdn.discordapp.com"
            || host == "media.discordapp.net"
            || host.ends_with(".discordapp.com")
            || host.ends_with(".discordapp.net"))
}

/// An attachment that is a MIDI file we are willing to remember.
fn midi_ref(a: &serenity::Attachment) -> Option<MidiRef> {
    if !is_midi(Some(&a.filename), a.content_type.as_deref()) {
        return None;
    }
    if u64::from(a.size) > MAX_MIDI_BYTES || !is_discord_cdn(&a.url) {
        return None;
    }
    Some(MidiRef {
        name: a.filename.clone(),
        key: a.url.clone(),
    })
}

/// Sends through a slash command's interaction, falling back to plain channel
/// messages once its token has expired (it lasts 15 minutes; a long render
/// outlives it), or straight to the channel for a `!ytp` message.
pub struct DiscordTransport {
    http: Arc<serenity::Http>,
    web: reqwest::Client,
    preset: &'static str,
    interaction: Option<serenity::CommandInteraction>,
    original: Mutex<Option<serenity::MessageId>>,
    used: AtomicBool,
}

impl DiscordTransport {
    fn channel(room: &str) -> BotResult<serenity::ChannelId> {
        room.parse::<u64>()
            .map(serenity::ChannelId::new)
            .map_err(|_| BotError::internal(format!("bad channel id {room:?}")))
    }

    fn message(id: &str) -> BotResult<serenity::MessageId> {
        id.parse::<u64>()
            .map(serenity::MessageId::new)
            .map_err(|_| BotError::internal(format!("bad message id {id:?}")))
    }

    fn failed(what: &str, e: impl std::fmt::Display) -> BotError {
        BotError::internal(format!("{what}: {e}"))
    }

    /// Has anything been sent to the interaction yet?
    fn answered(&self) -> bool {
        self.used.load(Ordering::SeqCst)
    }

    async fn channel_send(
        &self,
        channel: serenity::ChannelId,
        text: &str,
        reply_to: Option<&str>,
        file: Option<serenity::CreateAttachment>,
    ) -> BotResult<serenity::MessageId> {
        let mut builder = serenity::CreateMessage::new()
            .content(text)
            .allowed_mentions(no_mentions());
        if let Some(Ok(id)) = reply_to.map(Self::message) {
            builder = builder.reference_message((channel, id));
        }
        if let Some(file) = file {
            builder = builder.add_file(file);
        }
        channel
            .send_message(&self.http, builder)
            .await
            .map(|m| m.id)
            .map_err(|e| Self::failed("could not send a message", e))
    }
}

#[async_trait]
impl ChatTransport for DiscordTransport {
    fn upload_preset(&self) -> String {
        self.preset.to_string()
    }

    async fn send(&self, room: &str, text: &str, reply_to: Option<&str>) -> BotResult<String> {
        let channel = Self::channel(room)?;
        let mut first: Option<String> = None;
        for (i, chunk) in split_message(text).into_iter().enumerate() {
            let sent = match &self.interaction {
                Some(interaction) => {
                    let via_interaction = if !self.used.swap(true, Ordering::SeqCst) {
                        let r = interaction
                            .edit_response(
                                &self.http,
                                serenity::EditInteractionResponse::new()
                                    .content(&chunk)
                                    .allowed_mentions(no_mentions()),
                            )
                            .await;
                        if let Ok(m) = &r {
                            *self.original.lock().unwrap() = Some(m.id);
                        }
                        r
                    } else {
                        interaction
                            .create_followup(
                                &self.http,
                                serenity::CreateInteractionResponseFollowup::new()
                                    .content(&chunk)
                                    .allowed_mentions(no_mentions()),
                            )
                            .await
                    };
                    match via_interaction {
                        Ok(m) => Ok(m.id),
                        Err(e) => {
                            tracing::debug!(error = %e, "interaction reply failed; using the channel");
                            self.channel_send(channel, &chunk, None, None).await
                        }
                    }
                }
                None => {
                    self.channel_send(channel, &chunk, if i == 0 { reply_to } else { None }, None)
                        .await
                }
            }?;
            first.get_or_insert_with(|| sent.to_string());
        }
        Ok(first.unwrap_or_default())
    }

    async fn edit(&self, room: &str, message_id: &str, text: &str) -> BotResult<()> {
        let channel = Self::channel(room)?;
        let id = Self::message(message_id)?;
        let text = split_message(text).swap_remove(0);
        if let Some(interaction) = &self.interaction {
            let is_original = *self.original.lock().unwrap() == Some(id);
            let r = if is_original {
                interaction
                    .edit_response(
                        &self.http,
                        serenity::EditInteractionResponse::new()
                            .content(&text)
                            .allowed_mentions(no_mentions()),
                    )
                    .await
                    .map(|_| ())
            } else {
                interaction
                    .edit_followup(
                        &self.http,
                        id,
                        serenity::CreateInteractionResponseFollowup::new()
                            .content(&text)
                            .allowed_mentions(no_mentions()),
                    )
                    .await
                    .map(|_| ())
            };
            if r.is_ok() {
                return Ok(());
            }
        }
        channel
            .edit_message(
                &self.http,
                id,
                serenity::EditMessage::new()
                    .content(text)
                    .allowed_mentions(no_mentions()),
            )
            .await
            .map(|_| ())
            .map_err(|e| Self::failed("could not edit a message", e))
    }

    async fn react(&self, room: &str, message_id: &str, key: &str) {
        if self.interaction.is_some() {
            return;
        }
        let (Ok(channel), Ok(id)) = (Self::channel(room), Self::message(message_id)) else {
            return;
        };
        let r = channel
            .create_reaction(&self.http, id, serenity::ReactionType::Unicode(key.to_string()))
            .await;
        if let Err(e) = r {
            tracing::debug!(error = %e, "could not react to a message");
        }
    }

    async fn fetch_midi(&self, midi: &MidiRef) -> BotResult<MidiFile> {
        if !is_discord_cdn(&midi.key) {
            return Err(BotError::user(format!(
                "{} is not a Discord attachment",
                midi.name
            )));
        }
        let download = async {
            let mut resp = self.web.get(&midi.key).send().await?.error_for_status()?;
            if resp.content_length().is_some_and(|n| n > MAX_MIDI_BYTES) {
                return Ok(None);
            }
            let mut body = Vec::new();
            while let Some(chunk) = resp.chunk().await? {
                if (body.len() + chunk.len()) as u64 > MAX_MIDI_BYTES {
                    return Ok(None);
                }
                body.extend_from_slice(&chunk);
            }
            Ok::<_, reqwest::Error>(Some(body))
        };
        match download.await {
            Ok(Some(bytes)) => Ok(MidiFile {
                name: midi.name.clone(),
                bytes,
            }),
            Ok(None) => Err(BotError::user(format!(
                "{} is over {} MB",
                midi.name,
                MAX_MIDI_BYTES / (1 << 20)
            ))),
            Err(e) => Err(BotError::user(format!(
                "Couldn't download {} from Discord ({}).",
                midi.name,
                e.without_url()
            ))),
        }
    }

    async fn send_video(
        &self,
        room: &str,
        file: &EncodedFile,
        caption: &str,
        reply_to: Option<&str>,
    ) -> BotResult<()> {
        let channel = Self::channel(room)?;
        let data = tokio::fs::read(&file.path)
            .await
            .map_err(|e| BotError::internal(format!("could not read the video: {e}")))?;
        let caption = split_message(caption).swap_remove(0);
        let attachment = || serenity::CreateAttachment::bytes(data.clone(), file.filename.clone());
        let too_big = |e: &dyn std::fmt::Display| {
            BotError::user(format!(
                "The upload failed ({e}). Discord may be capping uploads below this video's size; try a shorter one (max=30)."
            ))
        };

        if let Some(interaction) = &self.interaction {
            self.used.store(true, Ordering::SeqCst);
            let r = interaction
                .create_followup(
                    &self.http,
                    serenity::CreateInteractionResponseFollowup::new()
                        .content(&caption)
                        .allowed_mentions(no_mentions())
                        .add_file(attachment()),
                )
                .await;
            match r {
                Ok(_) => return Ok(()),
                // A 413 is final; anything else may be the token having
                // expired, which the channel does not mind.
                Err(serenity::Error::Http(ref h)) if is_too_large(h) => return Err(too_big(&"too large")),
                Err(e) => tracing::debug!(error = %e, "interaction upload failed; using the channel"),
            }
        }
        self.channel_send(channel, &caption, reply_to, Some(attachment()))
            .await
            .map(|_| ())
            .map_err(|e| too_big(&e))
    }
}

fn is_too_large(e: &serenity::HttpError) -> bool {
    matches!(e, serenity::HttpError::UnsuccessfulRequest(r) if r.status_code.as_u16() == 413)
}

// ── slash commands ────────────────────────────────────────────────────────

const SLASH_HELP: &str = "\
**/ytp say** `text` — a video of the voice saying it (`subs:false` drops the captions)
**/ytp mv** — a MIDI file as a YTPMV, one tile per instrument. Attach the file, or leave it \
off to use the last one posted here
  `parts:` choose a part's word or tweak it, e.g. `lead=yeah kick=boom octave:bass=+1 chip=nes`
  `max:` the longest the video may be, in seconds
**/ytp mv-info** — what the song is and which word each part would use
**/ytp voices** — the voices available
**/ytp voice** `name` — switch voice (needs the permission)
**/ytp queue** — how many videos are waiting";

async fn author_roles(ctx: Context<'_>) -> Vec<String> {
    ctx.author_member()
        .await
        .map(|m| m.roles.iter().map(ToString::to_string).collect())
        .unwrap_or_default()
}

fn allowed(ctx: Context<'_>) -> bool {
    ctx.data().core.policy().discord_allowed(
        &ctx.author().id.to_string(),
        ctx.guild_id().map(|g| g.to_string()).as_deref(),
        &ctx.channel_id().to_string(),
    )
}

async fn tell(ctx: Context<'_>, text: &str) -> Result<(), Error> {
    ctx.send(
        poise::CreateReply::default()
            .content(text)
            .ephemeral(true)
            .allowed_mentions(no_mentions()),
    )
    .await?;
    Ok(())
}

/// Run `text` through the core as the slash command's user, with the first
/// status message replacing the "thinking..." placeholder.
async fn run_slash(
    ctx: Context<'_>,
    text: String,
    attachment: Option<serenity::Attachment>,
) -> Result<(), Error> {
    if !allowed(ctx) {
        return tell(ctx, "The bot isn't available to you here.").await;
    }
    let midi = match &attachment {
        None => None,
        Some(a) => match midi_ref(a) {
            Some(m) => Some(m),
            None => {
                return tell(
                    ctx,
                    "That doesn't look like a MIDI file (.mid, .midi or .kar, up to 2 MB).",
                )
                .await
            }
        },
    };
    let Context::Application(app) = ctx else {
        return Err("not an application command".into());
    };
    // Discord wants an answer within three seconds; the work takes far longer.
    ctx.defer().await?;

    let interaction = app.interaction.clone();
    let transport = DiscordTransport {
        http: ctx.serenity_context().http.clone(),
        web: ctx.data().web.clone(),
        preset: preset_for(guild_tier(&ctx.serenity_context().cache, ctx.guild_id())),
        interaction: Some(interaction.clone()),
        original: Mutex::new(None),
        used: AtomicBool::new(false),
    };
    let event = ChatEvent {
        platform: Platform::Discord,
        room: ctx.channel_id().to_string(),
        user_id: ctx.author().id.to_string(),
        user_name: ctx.author().name.clone(),
        direct: ctx.guild_id().is_none(),
        text,
        event_id: Some(interaction.id.to_string()),
        reply_to: None,
        midi,
        role_ids: author_roles(ctx).await,
        guild: ctx.guild_id().map(|g| g.to_string()),
    };
    ctx.data().core.handle(event, &transport).await;
    if !transport.answered() {
        let _ = transport.send(&ctx.channel_id().to_string(), "Done.", None).await;
    }
    Ok(())
}

async fn autocomplete_voice(
    ctx: Context<'_>,
    partial: &str,
) -> serenity::CreateAutocompleteResponse {
    let names = if allowed(ctx) {
        ctx.data().core.voice_names().await.unwrap_or_default()
    } else {
        Vec::new()
    };
    let partial = partial.to_lowercase();
    let choices = names
        .into_iter()
        .filter(|v| v.to_lowercase().contains(&partial))
        .take(25)
        .map(serenity::AutocompleteChoice::from)
        .collect();
    serenity::CreateAutocompleteResponse::new().set_choices(choices)
}

/// Make videos with the YTP machine.
#[poise::command(slash_command, subcommands("say", "mv", "mv_info", "voices", "voice", "queue", "help"))]
pub async fn ytp(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// A video of the voice saying a sentence.
#[poise::command(slash_command)]
async fn say(
    ctx: Context<'_>,
    #[description = "What to say"] text: String,
    #[description = "Show the words on the picture (default yes)"] subs: Option<bool>,
) -> Result<(), Error> {
    let prefix = ctx.data().core.prefix().to_string();
    let flag = if subs == Some(false) { "--no-subs " } else { "" };
    run_slash(ctx, format!("{prefix} say {flag}{text}"), None).await
}

/// Turn a MIDI file into a YTPMV.
#[poise::command(slash_command)]
async fn mv(
    ctx: Context<'_>,
    #[description = "The MIDI file (default: the last one posted in this channel)"]
    midi: Option<serenity::Attachment>,
    #[description = "Part words and settings, e.g. lead=yeah kick=boom octave:bass=+1 chip=nes"]
    parts: Option<String>,
    #[description = "Longest video, in seconds"]
    #[min = 1]
    #[max = 600]
    max: Option<u32>,
) -> Result<(), Error> {
    let prefix = ctx.data().core.prefix().to_string();
    let mut text = format!("{prefix} mv");
    if let Some(p) = parts.as_deref().filter(|p| !p.trim().is_empty()) {
        text.push(' ');
        text.push_str(p.trim());
    }
    if let Some(m) = max {
        text.push_str(&format!(" max={m}"));
    }
    run_slash(ctx, text, midi).await
}

/// What a MIDI file is and which word each part would use.
#[poise::command(slash_command, rename = "mv-info")]
async fn mv_info(
    ctx: Context<'_>,
    #[description = "The MIDI file (default: the last one posted in this channel)"]
    midi: Option<serenity::Attachment>,
) -> Result<(), Error> {
    let prefix = ctx.data().core.prefix().to_string();
    run_slash(ctx, format!("{prefix} mv info"), midi).await
}

/// The voices available.
#[poise::command(slash_command)]
async fn voices(ctx: Context<'_>) -> Result<(), Error> {
    let prefix = ctx.data().core.prefix().to_string();
    run_slash(ctx, format!("{prefix} voices"), None).await
}

/// Switch the voice for everyone.
#[poise::command(slash_command)]
async fn voice(
    ctx: Context<'_>,
    #[description = "The voice's name"]
    #[autocomplete = "autocomplete_voice"]
    name: String,
) -> Result<(), Error> {
    let prefix = ctx.data().core.prefix().to_string();
    run_slash(ctx, format!("{prefix} voice {name}"), None).await
}

/// How many videos are waiting.
#[poise::command(slash_command)]
async fn queue(ctx: Context<'_>) -> Result<(), Error> {
    let prefix = ctx.data().core.prefix().to_string();
    run_slash(ctx, format!("{prefix} queue"), None).await
}

/// What the bot can do.
#[poise::command(slash_command)]
async fn help(ctx: Context<'_>) -> Result<(), Error> {
    if !allowed(ctx) {
        return tell(ctx, "The bot isn't available to you here.").await;
    }
    tell(ctx, SLASH_HELP).await
}

// ── `!ytp` messages ───────────────────────────────────────────────────────

async fn on_message(
    ctx: &serenity::Context,
    data: &Data,
    msg: &serenity::Message,
) -> Result<(), Error> {
    if msg.author.bot || msg.webhook_id.is_some() {
        return Ok(());
    }
    let midi = msg.attachments.iter().find_map(midi_ref);
    let text = if data.settings.prefix_commands {
        msg.content.clone()
    } else {
        String::new()
    };
    if text.trim().is_empty() && midi.is_none() {
        return Ok(());
    }
    let transport = DiscordTransport {
        http: ctx.http.clone(),
        web: data.web.clone(),
        preset: preset_for(guild_tier(&ctx.cache, msg.guild_id)),
        interaction: None,
        original: Mutex::new(None),
        used: AtomicBool::new(false),
    };
    let event = ChatEvent {
        platform: Platform::Discord,
        room: msg.channel_id.to_string(),
        user_id: msg.author.id.to_string(),
        user_name: msg.author.name.clone(),
        direct: msg.guild_id.is_none(),
        text,
        event_id: Some(msg.id.to_string()),
        reply_to: msg
            .message_reference
            .as_ref()
            .and_then(|r| r.message_id)
            .map(|id| id.to_string()),
        midi,
        role_ids: msg
            .member
            .as_ref()
            .map(|m| m.roles.iter().map(ToString::to_string).collect())
            .unwrap_or_default(),
        guild: msg.guild_id.map(|g| g.to_string()),
    };
    let core = data.core.clone();
    tokio::spawn(async move {
        core.handle(event, &transport).await;
    });
    Ok(())
}

fn on_event<'a>(
    framework: poise::FrameworkContext<'a, Data, Error>,
    event: &'a serenity::FullEvent,
) -> poise::BoxFuture<'a, Result<(), Error>> {
    Box::pin(async move {
        let data = framework.user_data;
        match event {
            serenity::FullEvent::Message { new_message } => {
                on_message(framework.serenity_context, data, new_message).await?;
            }
            serenity::FullEvent::Ready { .. } | serenity::FullEvent::Resume { .. } => {
                data.status.connected();
            }
            serenity::FullEvent::ShardStageUpdate { event } => match event.new {
                serenity::ConnectionStage::Connected => data.status.connected(),
                serenity::ConnectionStage::Disconnected => data.status.failed("disconnected"),
                _ => {}
            },
            _ => {}
        }
        Ok(())
    })
}

fn on_error(error: poise::FrameworkError<'_, Data, Error>) -> poise::BoxFuture<'_, ()> {
    Box::pin(async move {
        match error {
            poise::FrameworkError::Command { error, ctx, .. } => {
                tracing::warn!(command = %ctx.command().qualified_name, %error, "a discord command failed");
            }
            other => {
                if let Err(e) = poise::builtins::on_error(other).await {
                    tracing::warn!(error = %e, "discord error handler failed");
                }
            }
        }
    })
}

/// The Discord bot: everything needed to run and reconnect it.
pub struct DiscordBot {
    settings: DiscordSettings,
    core: Arc<BotCore>,
    status: Arc<StatusHandle>,
}

impl DiscordBot {
    pub fn new(settings: DiscordSettings, core: Arc<BotCore>, status: Arc<StatusHandle>) -> Self {
        Self {
            settings,
            core,
            status,
        }
    }

    /// Run until the process stops. The gateway reconnects by itself; this
    /// loop covers a failed start (bad token, no network) and a client that
    /// gave up, so Discord being down never takes the video server with it.
    pub async fn run(self) {
        let mut backoff = 1u64;
        loop {
            let started = std::time::Instant::now();
            match self.session().await {
                Ok(()) => tracing::warn!("discord client stopped"),
                Err(e) => {
                    tracing::warn!(error = %e, "discord bot stopped");
                    self.status.failed(e.to_string());
                }
            }
            if started.elapsed() > Duration::from_secs(120) {
                backoff = 1;
            }
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(BACKOFF_MAX_SECS);
        }
    }

    async fn session(&self) -> Result<(), Error> {
        let web = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(60))
            .build()?;
        let data = Data {
            core: self.core.clone(),
            settings: Arc::new(self.settings.clone()),
            status: self.status.clone(),
            web,
        };

        let mut intents = serenity::GatewayIntents::GUILDS;
        if self.settings.message_content {
            intents |= serenity::GatewayIntents::GUILD_MESSAGES
                | serenity::GatewayIntents::DIRECT_MESSAGES
                | serenity::GatewayIntents::MESSAGE_CONTENT;
        }

        let framework = poise::Framework::builder()
            .options(poise::FrameworkOptions {
                commands: vec![ytp()],
                event_handler: on_event,
                on_error,
                ..Default::default()
            })
            .setup(move |ctx, ready, framework| {
                Box::pin(async move {
                    tracing::info!(user = %ready.user.name, "discord bot logged in");
                    poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                    Ok(data)
                })
            })
            .build();

        let mut client = serenity::ClientBuilder::new(&self.settings.token, intents)
            .framework(framework)
            .await?;
        client.start().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_messages_split_on_lines() {
        assert_eq!(split_message("hi"), vec!["hi"]);
        let line = "x".repeat(900);
        let text = format!("{line}\n{line}\n{line}");
        let parts = split_message(&text);
        assert_eq!(parts.len(), 2);
        assert!(parts.iter().all(|p| p.chars().count() <= MESSAGE_LIMIT));
        assert_eq!(parts.concat(), text);
        let solid = "é".repeat(4500);
        let parts = split_message(&solid);
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|p| p.chars().count() <= MESSAGE_LIMIT));
        assert_eq!(parts.concat(), solid);
    }

    #[test]
    fn only_discord_cdn_urls_are_fetched() {
        assert!(is_discord_cdn("https://cdn.discordapp.com/attachments/1/2/a.mid?ex=1"));
        assert!(is_discord_cdn("https://media.discordapp.net/attachments/1/2/a.mid"));
        assert!(!is_discord_cdn("http://cdn.discordapp.com/a.mid"));
        assert!(!is_discord_cdn("https://evil.example/a.mid"));
        assert!(!is_discord_cdn("https://cdn.discordapp.com@evil.example/a.mid"));
        assert!(!is_discord_cdn("https://cdn.discordapp.com.evil.example/a.mid"));
        assert!(!is_discord_cdn("https://127.0.0.1:8765/api/jobs"));
    }

    #[test]
    fn upload_preset_follows_the_boost_tier() {
        use super::serenity::PremiumTier as T;
        assert_eq!(preset_for(None), "discord-10mb");
        assert_eq!(preset_for(Some(T::Tier0)), "discord-10mb");
        assert_eq!(preset_for(Some(T::Tier1)), "discord-25mb");
        assert_eq!(preset_for(Some(T::Tier2)), "discord-50mb");
        assert_eq!(preset_for(Some(T::Tier3)), "discord-50mb");
    }
}