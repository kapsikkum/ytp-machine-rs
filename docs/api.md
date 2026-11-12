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
| GET | /api/jobs/:id/result | – | the finished `video/mp4` (Range supported; not done 409) | owner, or jobs.view_all |

Not yet implemented (later phases, UI may call and show "not available yet" on 501):
`POST /api/ytpmv/*`, `/api/projects/*`.
Server returns `501 { error: "not_implemented" }` for these (after the usual auth and
permission checks: generate.ytpmv / project.read for GET, project.write otherwise).

### Generating a YTP

`POST /api/ytp/generate` queues a job and returns at once with `{ job_id }`.

- `text`: what to say, at most 4000 characters (422 when empty or too long).
- `voice`: the name of an installed corpus; the first installed one when omitted (422 when unknown).
- `captions`: burn the words into the video (default `true`).
- `options`: an object, every value pulled into range and defaulted (never rejected):
  `speed` 0.5–2, `pitch` −12–12, `word_gap` 0–0.6, `sentence_pause` 0–2, `stretch` 0.02–0.35,
  `clean_takes` (true), `phrases` (true), `tone_match` 0–1, `pause_style` 0–2, `chaos` 0–1,
  `seed` (text of letters, digits, `.` and `-`, at most 64; the same seed gives the same picks,
  and one is invented when empty), plus `sing*` and `vocode`.
  `sing`/`vocode` are accepted but not rendered yet; the job gets a warning.
  A non-object `options` is 422.

Quotas come from the caller's roles (the most generous one wins):

- `max_concurrent_jobs`: jobs of one user running at once. `0` turns generating off (403).
  Further jobs wait in the queue, up to `max(4, 4 × max_concurrent_jobs)` active jobs per user,
  after which `429 rate_limited`.
- `max_render_seconds`: the longest video allowed. `0` turns generating off (403). A text whose
  video would be longer fails the job (`state: failed`, the plan still in `report`).
- The server runs at most `--job-workers` jobs at a time overall.

Jobs live in memory while active. Finished jobs are written to `<data-dir>/output/<id>.json` and
`<id>.mp4`, reloaded on startup, and the newest 200 are kept for 7 days.
A job somebody else owns is `404` unless they hold `jobs.view_all`. A token needs `generate.ytp`,
`generate.ytpmv` or `jobs.view_all` to see jobs.

`DELETE /api/jobs/:id` cancels a queued job at once, asks a running one to stop (it ends as
`cancelled` shortly after) and deletes a finished one along with its files. Cancelling or
deleting somebody else's job needs `jobs.cancel_any` (403 otherwise) and is audited as `job.cancel`.

Job events go to the WebSocket topics `job:<id>` and `jobs`. While running the message reads
`loading`, `choosing clips n/m`, `encoding` or `joining`; progress runs 0–2 % loading,
→ 40 % resolving, 40–95 % encoding, 96 % joining, 100 % done.

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
              result_url?: string /* "/api/jobs/<id>/result", only when done */ }
AuditEntry= { id: number, at: string, actor: string|null, action: string, target: string|null, detail: object|null, ip: string|null }
Corpus    = { name: string, words: number, clips: number, sources: number, size_bytes: number, installed_at: string }
```

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

## Binary

`ytp-machine serve [--bind 127.0.0.1:8765] [--data-dir ./data] [--ffmpeg <path>] [--job-workers 2] [--allowed-origin <url>]... [--trust-proxy] [--secure-cookies]`
(env: `YTP_BIND`, `YTP_DATA_DIR`, `YTP_FFMPEG`, `YTP_JOB_WORKERS`, `YTP_ALLOWED_ORIGINS`, `YTP_TRUST_PROXY`, `YTP_SECURE_COOKIES`).
`ytp-machine user create <name> --role admin [--password-stdin]` (CLI admin escape hatch), `ytp-machine user reset-password <name> [--password-stdin]`.
Without `--password-stdin` a random password is generated and printed.
UI is embedded from `web/build`. SPA fallback: unknown non-`/api` paths serve `index.html`.
