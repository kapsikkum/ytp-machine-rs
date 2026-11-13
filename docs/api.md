# API contract (v1 foundation)

Shared contract between the Rust server (`crates/`) and the SvelteKit UI (`web/`).
All JSON. All paths under `/api`. Errors: `{ "error": "<code>", "message": "<human text>" }`
with status 400/401/403/404/409/422/429/500 (and 501 for not-yet-implemented endpoints).
Error codes: `bad_request` (400, incl. malformed JSON), `too_large` (400), `unauthorized` (401),
`invalid_credentials` (401, login), `forbidden` (403, missing permission), `csrf_failed` (403),
`not_found` (404), `conflict` (409), `invalid` (422, validation / unknown permission id),
`rate_limited` (429), `internal` (500), `not_implemented` (501).
Auth is checked before the body is parsed, so an unauthorised request gets 401/403, never 422.

## Auth model

- Session cookie `ytp_session` (HttpOnly, SameSite=Lax, Secure when behind HTTPS).
- CSRF: every non-GET request with a cookie session must send header
  `X-CSRF-Token: <csrf>` (value from `/api/auth/state`). Bearer-token requests are exempt.
- API tokens: `Authorization: Bearer ytp_<random>`. Scoped to a subset of permissions.
- First run: if no users exist, the server prints a one-time setup URL to stdout
  (`/setup?token=...`). Setup creates the first admin.

### Permissions (string ids)

`generate.ytp`, `generate.ytpmv`, `studio.use`, `project.read`, `project.write`,
`project.share`, `export.render`, `export.hq`, `corpus.read`, `corpus.edit`,
`corpus.install`, `corpus.delete`, `jobs.view_all`, `jobs.cancel_any`,
`users.manage`, `roles.manage`, `tokens.create`, `settings.manage`, `audit.view`.

Built-in roles: `admin` (all), `editor`, `creator`, `guest`.

### Endpoints

| Method | Path | Body | Response | Permission |
|---|---|---|---|---|
| GET | /api/auth/state | – | `{ setup_required, user: User\|null, permissions: string[], csrf: string\|null }` | public |
| POST | /api/auth/setup | `{ token, username, password }` | `{ user, permissions, csrf }` + sets cookie (wrong token 403, already done 409) | setup token |
| POST | /api/auth/login | `{ username, password }` | `{ user, permissions, csrf }` + cookie | public (rate limited) |
| POST | /api/auth/logout | – | 204 | session |
| POST | /api/auth/password | `{ current, new }` (wrong current 403) | 204; other sessions signed out | session |
| GET | /api/auth/permissions | – | `string[]` (every permission id the server knows) | any logged-in user |
| GET | /api/admin/users | – | `User[]` | users.manage |
| POST | /api/admin/users | `{ username, password, roles: string[] }` | `User` | users.manage |
| PATCH | /api/admin/users/:id | `{ roles?, disabled?, password? }` | `User` | users.manage |
| DELETE | /api/admin/users/:id | – | 204 | users.manage |
| GET | /api/admin/roles | – | `Role[]` | roles.manage |
| POST | /api/admin/roles | `{ name, permissions, quota? }` | `Role` | roles.manage |
| PATCH | /api/admin/roles/:name | `{ permissions?, quota? }` | `Role` | roles.manage |
| DELETE | /api/admin/roles/:name | – | 204 (built-ins 409) | roles.manage |
| GET | /api/admin/audit?limit=&before= | – | `AuditEntry[]` newest first; `before` is an entry id (returns ids < before); `limit` default 100, max 1000 | audit.view |
| GET | /api/tokens | – | `Token[]` (own) | tokens.create |
| POST | /api/tokens | `{ name, permissions, expires_days? }` | `{ token: Token, secret }` (secret shown once) | tokens.create |
| DELETE | /api/tokens/:id | – | 204 | tokens.create |
| GET | /api/corpora | – | `Corpus[]` | corpus.read |
| POST | /api/corpora/install | multipart `file` (.tar.zst); optional `name` (default: slug of the file name), optional `replace=true` | `Corpus` (name taken 409, bad bundle 422) | corpus.install |
| DELETE | /api/corpora/:name | – | 204 | corpus.delete |
| GET | /api/health | – | `{ ok: true, version }` | public |

| POST | /api/ytp/generate | `{ text, voice?, captions?, options? }` | `{ job_id }` | generate.ytp |
| GET | /api/jobs/:id | – | `Job` | owner, or jobs.view_all |
| DELETE | /api/jobs/:id | – | 204 | owner, or jobs.cancel_any |
| GET | /api/jobs/:id/result | – | the finished file (Range supported; not done 409): `video/mp4` for a render, the preset's container for an export | owner, or jobs.view_all |
| GET | /api/encode/presets | – | `{ hq: boolean, presets: Preset[] }` (`allowed: false` marks an `hq` preset the caller may not use) | export.render |
| POST | /api/jobs/:id/export | `{ preset }` | `{ job_id }` (a new `encode` job; unknown preset 422, `hq` preset without export.hq 403, source not done or with no video file 409) | export.render (and export.hq for an hq preset); the source's owner, or jobs.view_all |
| GET | /api/admin/encoders | – | `{ ffmpeg, ok, encoders: { encoder, available, detail }[], presets }` (each hardware encoder tried with a test encode) | settings.manage |
| GET | /api/admin/bots | – | `{ service_user: string\|null, bots: { platform: "matrix"\|"discord", enabled, connected, last_error: string\|null, since: string\|null, detail: object }[] }`; `detail` is the non-secret configuration (homeserver, prefix, allow-list sizes...). Tokens and passwords are never returned. Empty `bots` until the server has started the bots | settings.manage |

Projects (the Studio's timeline documents), plans and previews are described under
"Projects and the timeline model" below.

### YTPMV (a MIDI file sung by a corpus)

Every route needs `generate.ytpmv`; `POST /api/ytpmv/render` also needs `export.render` (403
otherwise). MIDI files belong to the user who uploaded them (another user's id is a 404).

| Method | Path | Body / query | Answer |
|---|---|---|---|
| POST | /api/ytpmv/midi | multipart `midi` (or `file`), optional `voice` | `Analysis` (the file is parsed before it is kept: not MIDI or no notes 422; over 2 MB or 200 files per user refused) |
| GET | /api/ytpmv/library | – | `MidiEntry[]`, newest first |
| GET | /api/ytpmv/midi/:id?voice= | – | `Analysis`, with words suggested from `voice` (the first installed corpus by default; none installed: still works, no words) |
| PATCH | /api/ytpmv/midi/:id | `{ title }` (1–80 characters) | `{ midi_id, title }` |
| DELETE | /api/ytpmv/midi/:id | – | 204 (409 while a render of it is queued or running) |
| GET | /api/ytpmv/midi/:id/notes?start=&end=&parts=a,b | – | `{ parts: {id: [start, dur, pitch][]}, counts, sampled }` the notes of the visible parts in a range, thinned to a bounded number |
| GET | /api/ytpmv/midi/:id/part/:part?voice= | – | `{ id, corpus, recommended, settings, noises, units }` the suggestion for one part under another voice |
| POST | /api/ytpmv/sample | `{ text, take?, corpus?, hit? }` | what the word is as an instrument (clip, pitch) |
| POST | /api/ytpmv/sample/audio | same | the trimmed clip as `audio/wav` |
| POST | /api/ytpmv/preview | `{ midi_id, part, seconds?, chip?, synth?, corpus? }` | one part alone, as `audio/wav` |
| POST | /api/ytpmv/preview-mix | `{ midi_id, parts?, options?, start?, seconds?, part_id?, note?, notes?, voice? }` | the mix as `audio/wav`, no picture |
| POST | /api/ytpmv/render | `{ midi_id, parts?, options?, voice? }` | `{ job_id }` |

`parts` is a list of per-part choices, each `{ id, corpus?, text?, take?, takes?, pool?, octave?
(−4–4), transpose?, volume? (0–2), pan?, mode?, sustain?, tone?, program?, hit?, haas?, mute?,
visible? }`. Anything left out takes the suggestion in `Analysis.parts[].settings`; every value is
pulled into range, never rejected. `options` is the song-wide set: `max_seconds`, `speed`
(0.25–4), `transpose`, `vary` (`off|rotate|random|ultra`), `balance`, `accurate`, `jitter`,
`chip` (see `Analysis.instruments.chips`), `synth` (no voice: every part is a chip, with
oscilloscope tiles, no corpus needed), `flip`, `flash`, `dim`, `labels`, `layout`
(`balanced|classic|two-rows|two-columns|strips|featured`), `media_fit` (`contain|cover`),
`center_tiles`, `draft`, `tile_order`.

Previews are synchronous and in-process (nothing is queued): the answer is the WAV itself, and
what happened is in the `X-Ytpmv-Info` header as JSON (non-ASCII escaped as `\uXXXX`):
`{ from, duration, problems, options, parts: [{ id, corpus, volume, pan }] }`. `seconds: null` on
preview-mix means to the end of the song; with `part_id` only that part sounds and the preview
starts at its first note. A typical song mixes in about half a second once its sounds have been
built (`GET /api/ytpmv/midi/:id` does that work the first time, a few seconds for a corpus).

`render` queues a job like `/api/ytp/generate` (same quotas, queue and WebSocket events). The job
record has `kind: "ytpmv"`, `text` = the song title, `voice` = the default corpus and `options` =
`{ midi_id, parts, options }`. Progress reads `measuring words`, `choosing sounds`, `playing notes`, `loading clips`, `drawing frames` and
`encoding` (0–100 %); `GET /api/jobs/:id/result` is a `video/mp4` whose size follows the role's
`max_height` (1920×1080 up to 1080p, 1280×720, 852×480 …; the tiles are laid out at that size rather
than rendered at 1080p and scaled). Its `report` is
`{ duration, size: [w, h], song, options (as used), parts: [...], problems: string[] }`.
`max_render_seconds` fails a longer render (`your limit is N s`).

`requests_per_minute` is spent by `sample`, `sample/audio`, `preview`, `preview-mix` and `render`;
uploading, renaming and deleting a MIDI file are not counted.

### Generating a YTP

`POST /api/ytp/generate` queues a job and returns at once with `{ job_id }`.

- `text`: what to say, at most 4000 characters (422 when empty or too long).
- `voice`: the name of an installed corpus; the first installed one when omitted (422 when unknown).
- `captions`: burn the words into the video (default `true`).
- `options`: an object, every value pulled into range and defaulted (never rejected):
  `speed` 0.5–2, `pitch` −12–12, `word_gap` 0–0.6, `sentence_pause` 0–2, `stretch` 0.02–0.35,
  `clean_takes` (true), `phrases` (true), `tone_match` 0–1, `pause_style` 0–2, `chaos` 0–1,
  `seed` (text of letters, digits, `.` and `-`, at most 64; the same seed gives the same picks,
  and one is invented when empty).
  The singing and vocoder options (`sing`, `sing_*`, `vocode`, and the `{vocode}` effect in the
  text) were removed pending a rework: a request that includes any of them is `422` with a message
  saying so, never silently ignored. A non-object `options` is 422.

Quotas come from the caller's roles (the most generous one wins):

- `max_concurrent_jobs`: jobs of one user running at once. `0` turns generating off (403).
  Further jobs wait in the queue, up to `max(4, 4 × max_concurrent_jobs)` active jobs per user,
  after which `429 rate_limited`.
- `max_render_seconds`: the longest video allowed. `0` turns generating off (403). A text whose
  video would be longer fails the job (`state: failed`, the plan still in `report`).
- `requests_per_minute`: a token bucket per user (a minute's worth of requests, refilled evenly)
  spent by every request that starts generation (`POST /api/ytp/generate`, and the YTPMV routes
  that make sound or video, listed above).
  Out of tokens is `429 rate_limited` with a `Retry-After` header (seconds); `0` turns those
  requests off (403). Reading jobs, corpora and the rest is not counted.
- The server runs at most `--job-workers` jobs at a time overall.

Per corpus, generating also reads what the Python app keeps with a corpus: `pronunciations.csv`
(the corpus's own, from its bundle, over `<data-dir>/pronunciations.csv` for every corpus; rows are
`word,ARPAbet | words that sound right | =letters | =skip`) changes how that corpus's words and
the words asked for are pronounced, and the `splice_recipes` table decides how a missing word is
built from pieces (a recipe saved for the corpus wins over the built-in one, a piece cut by hand is
played exactly). A cut-clip and decoded-audio cache under `<data-dir>/cache/clips`
(`--clip-cache-mb`, default 500, 0 = off; least recently used files go first) makes a repeated
sentence render faster. It changes nothing about what is chosen; the picture of a cached cut
differs from an uncached render only by one more near-lossless encode (about 53 dB PSNR).

Jobs live in memory while active. Finished jobs are written to `<data-dir>/output/<id>.json` and
`<id>.mp4`, with the intermediate master kept beside it as `<id>.master.mp4` (that is what an
export re-encodes), reloaded on startup, and the newest 200 are kept for 7 days.

`POST /api/jobs/:id/export` queues an `encode` job that finishes a finished job again with another
preset (see `GET /api/encode/presets`), without rendering it again. Its result is served at
`/api/jobs/:id/result` in the preset's container (`video/mp4`, `image/gif`, `audio/mp4`, `audio/ogg`,
`video/x-matroska`); `result_file` names the file when it is not `<id>.mp4`. Deleting either job
removes its own result, and deleting the source also removes the master an export would need.

A job somebody else owns is `404` unless they hold `jobs.view_all`. A token needs `generate.ytp`,
`generate.ytpmv` or `jobs.view_all` to see jobs.

`DELETE /api/jobs/:id` cancels a queued job at once, asks a running one to stop (it ends as
`cancelled` shortly after) and deletes a finished one along with its files. Cancelling or
deleting somebody else's job needs `jobs.cancel_any` (403 otherwise) and is audited as `job.cancel`.

Job events go to the WebSocket topics `job:<id>` and `jobs`. While running the message reads
`loading`, `choosing clips n/m`, `encoding`, `joining` or `compressing`; progress runs 0–2 % loading,
→ 40 % resolving, 40–95 % encoding, 96 % joining, 95–99 % compressing the master into the result,
100 % done. An export job reports `encoding as <preset>` over the same 0–100 %.

"session" above means a cookie session; API tokens get 403 there. Admin safeguards: the last
enabled admin cannot be demoted, disabled or deleted (409), you cannot delete yourself (409),
and the `admin` role's permissions are fixed (PATCH permissions → 409).

### Types

```ts
User      = { id: number, username: string, roles: string[], disabled: boolean, created_at: string }
Role      = { name: string, builtin: boolean, permissions: string[], quota: Quota }
Quota     = { max_concurrent_jobs: number, max_render_seconds: number, max_height: number, requests_per_minute: number }
Token     = { id: number, name: string, permissions: string[], created_at: string, expires_at: string|null, last_used_at: string|null }
Job       = { id: string, owner: number, owner_name: string, state: "queued"|"running"|"done"|"failed"|"cancelled",
              progress: number /* 0..1 */, message?: string, created_at: string, started_at?: string, finished_at?: string,
              text: string, voice: string, captions: boolean, options: object /* as used, seed included */,
              report?: { found: string[], spliced: string[], missing: string[], runs: string[],
                         tokens: { word, status: "found"|"run"|"spliced"|"missing", clips?: number[], approx?: true,
                                   stretched?: string, voice?: string, shown?: string }[] },
              timeline?: { i: number /* index in report.tokens */, start: number, end: number }[],
              duration?: number, size_bytes?: number, error?: string, warnings: string[],
              result_url?: string /* "/api/jobs/<id>/result", only when done */,
              result_file?: string /* e.g. "<id>.gif"; absent means "<id>.mp4" */,
              kind?: "ytpmv" | "encode" /* absent for a sentence; ytpmv report is the YTPMV report above */ }
Preset    = { name: string, title: string, container: "mp4"|"mkv"|"gif"|"m4a"|"ogg",
              video_codec: "h264"|"h265"|"gif"|"none", audio_codec: "aac"|"opus"|"none",
              quality: "crf"|"target_size_mb"|"bitrate", crf: number|null, target_mb: number|null,
              bitrate_kbps: number|null, audio_kbps: number, max_width: number, max_height: number,
              fps_cap: number, gop: number, hw: "auto"|"never", hq: boolean, allowed: boolean }
MidiEntry = { midi_id: string, title: string, duration: number, bpm: number, key: string, time_signature: string,
              tempo_changes: number, parts: number, drums: number, notes: number, description: string,
              bytes: number, added: number /* unix seconds */ }
Analysis  = { midi_id, title, duration, bpm, key, key_confidence, time_signature, tempo_changes, description,
              voice: string, corpus: string /* "" when none is installed */,
              noises: {name: count}, units: {name: count} /* sounds the voice has besides words */,
              instruments: { machines: {chip: {name, melodic: [tone, about][], drums: [tone, about][]}},
                             machine_of, voice: [tone, about][], synth: string[], programs: string[],
                             chips: {chip: about} },
              parts: { id, name, role, instrument, is_drums, notes, range, range_midi, median_midi, pan,
                       polyphony, first,
                       recommended: { text, take, takes?, pool?, octave, mode, score, pitch, alternatives:
                                      {text, take, score}[] },
                       settings: /* the part settings the render would use */,
                       chip_tones, chip_tones_synth: {chip: tone},
                       activity: [start, dur, pitch][] /* thinned */, program_table?: string }[],
              program_tones: {table: {chip: tone[] /* per General MIDI program */}} }
AuditEntry= { id: number, at: string, actor: string|null, action: string, target: string|null, detail: object|null, ip: string|null }
Corpus    = { name: string, words: number, clips: number, sources: number, size_bytes: number, installed_at: string }
```

## Projects and the timeline model

One document, `Project`, is what both generators produce and the Studio edits
(`crates/ytp-core/src/timeline.rs`; versioned: `version` is 1 and any other value is refused).
Times are seconds.

```jsonc
Project {
  version: 1, id?: number, owner?: number,       // id/owner are filled in by the server
  name: string, corpus: string,                   // corpus = default voice
  fps: 25, width: 480, height: 270,
  captions: boolean,                              // burn word captions in
  generator?: { kind: "ytp"|"ytpmv", request: object, seed: string },   // how it was made
  tracks: Track[]
}
Track { id, kind: "video"|"audio", name, mute, solo, volume /* 0..4, linear */,
        part?: string, generated: boolean,        // generated: a readout of the song renderer
        clips: Clip[] }
Clip  { id, start, duration,                      // duration is authoritative on the timeline
        source: ClipSource, effects: Effect[], label?: string,
        caption?: { text, words: [text, start, end][] },   // word times in *source* seconds
        place?: { x, y, w, h } }                   // frame fractions (0..1): box corner and size; only when composited over another clip; absent = the renderer's default box
ClipSource =
  { type: "corpus_clip", corpus, clip_id?, file?, word?, take?, in, out }  // file is relative to the corpus
| { type: "media", asset, in, out }                                         // an uploaded asset id
| { type: "synth", chip, notes: [{ start, dur, pitch, velocity }] }        // chip = a tone name
| { type: "generated", kind: "ytp"|"ytpmv", prompt, options, seed, corpus? } // must be expanded before rendering (not yet possible)
Effect = speed{factor} | stretch{factor} | reverse | pitch{semitones} | volume{db} | stutter
       | mirror | glitch{name} | fade_in{seconds} | fade_out{seconds} | pause{seconds} | subword
```

A `place` on the bottom layer of a composite (or in a project with a single video track) is inert — the
bottom layer always fills the frame — and it applies per overlap span, so the box shows only where clips
from different tracks overlap.

Validation (422 with the reason): ids are 1–64 of `A-Za-z0-9_-` and unique; `start >= 0`,
`duration > 0`, `in < out`; effect parameters in range (speed 0.1–10, stretch 0.1–16, pitch ±48,
volume −80…+40 dB); at most 256 tracks and 50 000 clips; a synth clip lives on an audio track.

Two kinds of project exist. A **sequence** (a sentence plan, or what the Studio edits) is video
clips end to end on the video tracks, gaps filled with black and silence, and audio clips laid over
it; video clips on the same track may not overlap, while clips on different tracks are composited
picture-in-picture (half width, bottom right, later track on top, without its sound or captions).
A **song** (`generator.kind = "ytpmv"`, tracks with `generated: true`) is a readout of the song
renderer: one track per part, one clip per note. It is played by running the song renderer again
with each track's mute/solo/volume put on its part; editing individual notes is not possible yet.

| Method | Path | Body | Response | Permission |
|---|---|---|---|---|
| GET | /api/projects | – | `ProjectMeta[]` (owned and shared with you, newest first): `{ id, owner, owner_name, name, kind: "sequence"\|"song", duration, clips, revision, access: "owner"\|"write"\|"read", created_at, updated_at }` | project.read |
| POST | /api/projects | `{ project, name? }` | `StoredProject` = `ProjectMeta` + `project` (422 if invalid; 409 at 200 projects; 16 MB limit) | project.write |
| GET | /api/projects/:id | – | `StoredProject` (not yours and not shared: 404) | project.read |
| PATCH | /api/projects/:id | `{ project?, name?, revision? }` | `StoredProject` with `revision + 1`; a `revision` that is not the stored one is 409 (lost update); a read-only share is 403 | project.write; owner or write share |
| DELETE | /api/projects/:id | – | 204 | project.write; owner only (else 403) |
| GET | /api/projects/:id/shares | – | `{ user_id, username, access: "read"\|"write" }[]` | project.share; owner only |
| POST | /api/projects/:id/shares | `{ username, access: "read"\|"write" }` | the shares (unknown user, yourself, other access: 422) | project.share; owner only |
| DELETE | /api/projects/:id/shares/:username | – | 204 | project.share; owner only |
| POST | /api/projects/:id/render | – | `{ job_id }`: a `project` job (full quality, through the sentence renderer and the shared encoder, master kept for `/export`); a song project runs the song renderer | export.render; any access |
| POST | /api/ytp/plan | the body of `/api/ytp/generate`, plus `save?: bool`, `name?` | `{ project, report }`; with `save`, `{ id, saved: StoredProject, report }` (needs project.write too). Nothing is rendered: the same text, voice and `options.seed` give the plan the job would render | generate.ytp |
| POST | /api/ytpmv/plan | the body of `/api/ytpmv/render`, plus `save?`, `name?` | `{ project }` or `{ id, saved }`: one track per part, one clip per note | generate.ytpmv |
| POST | /api/preview | `{ project_id } \| { project }`, `start?` (0), `duration?` (10, at most 30), `tracks?: [{ id, mute?, solo?, volume? }]` | the window as `video/mp4` (480p, H.264 + AAC); headers `X-Preview-Cache` (`hit`\|`miss`), `X-Preview-Ms`, `X-Preview-Info` (JSON: kind, clips, proxied...). A window after the end of the project is 422; the end is cut | studio.use (+ project.read for `project_id`) |

Sharing: the owner (with project.share) gives another user `read` or `write`; the project then
appears in their list. A project that is not yours and not shared with you is a 404, for admins too.
`write` lets someone PATCH it (they still need project.write); only the owner deletes or re-shares.
Sharing changes and deletes are in the audit log (`project.share`, `project.unshare`,
`project.delete`). A sharee who renders a project runs it as their own job; a song plays from the
*owner's* MIDI library.

### Preview

`POST /api/preview` is one request, one finished window, no streaming: the client asks for aligned
8 s windows, plays the current one and fetches the next into a second `<video>`, swapping at the
seam (the Studio does this: `web/src/lib/components/studio/PreviewPane.svelte`). Sequence projects
render the window's clips in one fast ffmpeg call (ultrafast H.264, CRF 30, only the window kept)
on top of preview proxies: the first preview of a source reads the original, and an all-intra 480p
copy (`preview-proxy` preset, sources up to 30 min, LRU-capped at 2 GB under
`<data>/cache/proxy`) is built in the background for the ones after. Song projects play only the
window through the song mixer and decode only the takes the window shows. Finished windows are
cached on disk (`<data>/cache/preview`, 256 MB, keyed by the clips inside the window, the mixer,
the corpus versions and the window), so replays and re-scrubs are instant and edits elsewhere in a
project do not invalidate them. At most three previews render at once; previews are not counted
against `requests_per_minute`.

## WebSocket `/api/ws`

Authenticated by the session cookie (or `?token=` bearer secret). Origin is checked: a browser
`Origin` must equal the request's own host (`Host` header) or be listed in `YTP_ALLOWED_ORIGINS`
(comma list, also `--allowed-origin`). For Vite dev on another port set
`YTP_ALLOWED_ORIGINS=http://localhost:5173`. Requests without an `Origin` (non-browser) pass.
Foreign origin → 403 before the upgrade.
Messages are JSON `{ "type": ..., ... }`.

Server → client: `hello { user, permissions }`, `pong`, `job { id: string, state: queued|running|done|failed|cancelled, progress: 0..1, message? }`,
`corpus { event: installed|removed, name }` (sent to everyone with corpus.read), `error { message }`.
Client → server: `ping` → `pong`; `subscribe { topic }` / `unsubscribe { topic }`.
Topics: `job:<id>` (that job, if yours or you have jobs.view_all), `jobs` (all jobs visible to you).

## Chat bots

`ytp-machine serve` runs a Matrix and/or Discord bot in the same process when the environment
configures one (setup: README, "Chat bots"). The bots are not an HTTP API; they call the job
manager, MIDI library and encoder directly as a *service account*. Jobs they start are owned by
that account (so they show up under it in `/api/jobs/:id`, subject to its quota), while what the
chat person may ask for is decided separately by the app role their chat identity maps to.

Environment (all optional; a platform is on when its credentials are set, or forced with
`YTP_MATRIX_BOT=1|0` / `YTP_DISCORD_BOT=1|0`):

| Variable | Meaning |
|---|---|
| `YTP_BOT_USER` | service account the bots run jobs as (default `bot`; must exist, e.g. `user create bot --role creator`) |
| `YTP_BOT_DEFAULT_ROLE` | role for allowed chat users with no mapping (default `guest`) |
| `YTP_BOT_PREFIX` | text-command prefix (default `!ytp`) |
| `YTP_BOT_MV_MAX_SECONDS` | longest song a chat command may make (default 120) |
| `YTP_MATRIX_HOMESERVER`, `YTP_MATRIX_USER` | `https://matrix.example.org`, `@ytp:example.org` |
| `YTP_MATRIX_PASSWORD` \| `YTP_MATRIX_TOKEN` | password login (session kept in `<data>/matrix/session.json`) or an access token |
| `YTP_MATRIX_ROOMS`, `YTP_MATRIX_USERS` | allow-lists (room ids; user ids, `*:server` for a whole server). Invites are accepted only from allowed users/rooms |
| `YTP_MATRIX_PREFIX`, `YTP_MATRIX_DEVICE_NAME` | prefix override; device name for a password login |
| `YTP_DISCORD_TOKEN` | bot token |
| `YTP_DISCORD_GUILDS`, `YTP_DISCORD_CHANNELS`, `YTP_DISCORD_USERS` | allow-lists (ids); any match is enough |
| `YTP_DISCORD_ROLES` | `discord-role-id=app-role,...` mapping |
| `YTP_DISCORD_DEFAULT_ROLE`, `YTP_DISCORD_PREFIX` | role / prefix overrides |
| `YTP_DISCORD_PREFIX_COMMANDS` | also answer `!ytp ...` messages (needs the Message Content intent) |
| `YTP_DISCORD_MESSAGE_CONTENT` | ask for the Message Content intent so MIDI files posted in a channel are remembered |

Commands: `say`, `mv`, `mv info`, `voices`, `voice <name>`, `queue`, `help` — as `!ytp <command>`
on Matrix, and as slash commands on Discord (`/ytp say text [subs]`, `/ytp mv [midi] [parts]
[max]`, `/ytp mv-info [midi]`, `/ytp voices`, `/ytp voice name`, `/ytp queue`, `/ytp help`).
Results are encoded with the `matrix` preset, or `discord-10mb` / `discord-25mb` / `discord-50mb`
by the guild's boost tier. Permissions needed by the chat user's mapped role: `say` generate.ytp;
`mv` generate.ytpmv + export.render; `mv info` generate.ytpmv; `voices` corpus.read; `voice`
corpus.edit (the bots share one voice, in memory, reset on restart).

## Binary

`ytp-machine serve [--bind 127.0.0.1:8765] [--data-dir ./data] [--ffmpeg <path>] [--job-workers 2] [--allowed-origin <url>]... [--trust-proxy] [--secure-cookies] [--clip-cache-mb 500]`
(env: `YTP_BIND`, `YTP_DATA_DIR`, `YTP_FFMPEG`, `YTP_JOB_WORKERS`, `YTP_ALLOWED_ORIGINS`, `YTP_TRUST_PROXY`, `YTP_SECURE_COOKIES`, `YTP_CLIP_CACHE_MB`).
`ytp-machine user create <name> --role admin [--password-stdin]` (CLI admin escape hatch), `ytp-machine user reset-password <name> [--password-stdin]`.
Without `--password-stdin` a random password is generated and printed.
UI is embedded from `web/build`. SPA fallback: unknown non-`/api` paths serve `index.html`.
