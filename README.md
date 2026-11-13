# ytp-machine

Rust rewrite of the "michael-rosen-says" YTP generator: splices word clips from
a speech corpus into YouTube Poop videos and YTPMV music, behind a multi-user
web UI (SvelteKit, embedded in the binary).

Phase 1: server foundation, accounts/roles/tokens, corpus install/list/delete.
Phase 2 (this): text and splice logic ported from the Python app, ffmpeg rendering, a job
queue, and `POST /api/ytp/generate` with the `/ytp` page working end to end.
YTPMV and projects still answer 501. API contract: [`docs/api.md`](docs/api.md).

Rendering needs `ffmpeg` on `PATH` (tested with 8.0.1), or `--ffmpeg <path>`.
Not ported yet: forced alignment (cuts use the corpus's stored phones). Singing and the
vocoder were removed pending a rework (requests that ask for them get a 422).
Per-corpus `pronunciations.csv`, saved splice recipes and a clip cache are in.

## Layout

- `crates/ytp-text`: tokenizer, CMUdict phonemes, number words, effects markup
- `crates/ytp-splice`: clip choice, phoneme splicing, timeline (Python-compatible RNG)
- `crates/ytp-media`: the ffmpeg renderer
- `tools/text/export_data.py`: regenerates the vendored CMUdict/tables and test fixtures

- `crates/ytp-core`: shared types (errors, events, timeline model)
- `crates/ytp-auth`: users, roles, permissions, sessions, API tokens, audit log
- `crates/ytp-corpus`: corpus bundles (`.tar.zst`) and read-only corpus databases
- `crates/ytp-server`: the `ytp-machine` binary (axum)
- `web/`: SvelteKit UI; `web/build` is embedded at compile time

## Run

```sh
cargo run -p ytp-server -- serve --data-dir ./data
```

On first start the server prints a one-time `http://127.0.0.1:8765/setup?token=...`
URL for creating the admin account. Other commands:

```sh
cargo run -p ytp-server -- user create alice --role editor
cargo run -p ytp-server -- user reset-password alice
```

For UI development with Vite on another port, allow its origin for the WebSocket:
`YTP_ALLOWED_ORIGINS=http://localhost:5173`.

Build the UI (`npm run build` in `web/`) before a release build of the server.
If `web/build` is missing the server still compiles and shows a "UI not built" page.

## Studio

The Studio is the editor at `/`; `/?project=<id>` opens a saved project. On `/ytp` and
`/ytpmv`, **Open in Studio** calls the plan endpoint (`POST /api/ytp/plan` /
`POST /api/ytpmv/plan` with `save: true`) and takes you to the new project, so nothing
renders until you ask for it. Projects are listed at `GET /api/projects`; saving goes
through `PATCH /api/projects/:id` with the `revision` you loaded, so a stale edit is
refused instead of overwriting someone else's.

Playback is a preview, not a stream: `POST /api/preview` returns one finished MP4 per aligned
window (the Studio asks for 8 s, the API allows up to 30), and the next one is fetched while the
current one plays. Windows
are cached on disk: song windows render at about 10× realtime, a YTP window takes 1–2.5 s
the first time and 1–60 ms once cached.

Editing is per track (mute/solo) with an effects panel for the selected clip: speed
(0.1–10), stretch (0.1–16), pitch (±48 semitones), volume (−80..40 dB), fades and pauses
(0–60 s), reverse, stutter, flip, subword, and the 11 glitches. The server checks every
value. Undo/redo is on the toolbar and on Ctrl+Z, Ctrl+Shift+Z and Ctrl+Y, with a bounded
200-state history (`History` in `crates/ytp-core/src/history.rs`).

To keep a result at full quality, **export** on a finished `/ytp` or `/ytpmv` result
starts `POST /api/jobs/:id/export` with a preset from `GET /api/encode/presets`; the file
is served at `GET /api/jobs/:id/result`. `hq` presets need the `export.hq` permission and
are greyed out without it. A whole project renders with `POST /api/projects/:id/render`.

Known limits: overlapping video clips on one track are refused, `generated` clip sources
cannot render yet, song projects are editable at track level only (notes are read-only),
and preview windows show the track mix rather than every per-clip effect.

## Chat bots

`serve` can also run a Matrix bot and a Discord bot in the same process. Neither is needed; each
turns on when its credentials are in the environment. A bot that can't connect (wrong token,
network down) only logs, shows up in `GET /api/admin/bots` with its last error, and retries with
a growing pause; it never stops the server. Full variable list: [`docs/api.md`](docs/api.md#chat-bots).

Both bots run jobs as a **service account**, and decide what a chat person may ask for from the
app role their chat identity maps to (`YTP_BOT_DEFAULT_ROLE`, `guest` unless set; on Discord
`YTP_DISCORD_ROLES` can map Discord roles to app roles). The built-in `guest` can `say` and list
voices but not make songs (`mv` needs `generate.ytpmv` + `export.render`; `voice` needs
`corpus.edit`), so set the default role to `creator` or `editor` for a trusted server.

```sh
# the account jobs run as (default name "bot"; YTP_BOT_USER to change). Give it a role that has
# generate.ytp, generate.ytpmv and export.render.
ytp-machine user create bot --role creator
```

### Discord

1. <https://discord.com/developers/applications> → **New Application**.
2. **Bot** tab → **Reset Token**, copy it into `YTP_DISCORD_TOKEN`.
   Intents: none are needed for slash commands. To also answer `!ytp ...` messages, and to
   remember MIDI files posted in a channel so `/ytp mv` works without attaching one, switch on
   **Message Content Intent** here and set `YTP_DISCORD_PREFIX_COMMANDS=1` (or
   `YTP_DISCORD_MESSAGE_CONTENT=1` for just the MIDI memory). Switching it on in the code but not
   in the portal makes Discord refuse the connection (the status shows "Disallowed intents").
3. **OAuth2 → URL Generator**: scopes `bot` and `applications.commands`; bot permissions *View
   Channels*, *Send Messages*, *Attach Files*, *Read Message History* (and *Add Reactions* for the
   `!ytp` 👀 acknowledgement). Open the generated URL to add the bot to your server.
4. Environment (turn on **Developer Mode** in Discord to copy ids):

   ```sh
   YTP_DISCORD_TOKEN=...
   YTP_DISCORD_GUILDS=123456789012345678        # servers / channels / users that may use it;
   YTP_DISCORD_CHANNELS=...                     # any match is enough, none set = everyone
   YTP_DISCORD_ROLES=111...=editor,222...=creator
   YTP_BOT_DEFAULT_ROLE=creator
   ```

Commands appear as `/ytp say`, `/ytp mv`, `/ytp mv-info`, `/ytp voices`, `/ytp voice`,
`/ytp queue`, `/ytp help`. They register globally on start (Discord can take a few minutes to show
new ones). `/ytp mv` takes an attached `.mid` (up to 2 MB), or uses the last MIDI posted or used
in the channel (remembered until restart). Results upload at 10 MB, or 25/50 MB by the server's
boost level. Note that Discord slash-command interactions expire after 15 minutes, after which
status messages fall back to plain channel messages.

### Matrix

1. Make an account for the bot on your homeserver (register normally, or with your server's admin
   tooling). It does not need to be an admin.
2. Environment:

   ```sh
   YTP_MATRIX_HOMESERVER=https://matrix.example.org
   YTP_MATRIX_USER=@ytp:example.org
   YTP_MATRIX_PASSWORD=...        # or YTP_MATRIX_TOKEN=<access token>
   YTP_MATRIX_USERS=@you:example.org,*:friends.example   # who may use it / invite it
   YTP_MATRIX_ROOMS=!abc:example.org                    # rooms it answers in
   ```

   With a password the login is saved in `<data>/matrix/session.json` so restarts reuse the same
   device. With neither list set the bot joins and answers anyone who invites it.
3. Invite the bot to an **unencrypted** room (end-to-end encryption isn't supported: it leaves an
   encrypted room it is invited to, and says so once in one it is already in). It only joins
   invites from allowed users or rooms. Then `!ytp help`; a MIDI file posted in the room (or
   replied to) is what `!ytp mv` plays. In a one-to-one room the prefix is optional and plain
   text means "say this". Videos upload with the `matrix` preset.

## Test

```sh
cargo test
cargo clippy --all-targets
```
