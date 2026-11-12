# ytp-machine

Rust rewrite of the "michael-rosen-says" YTP generator: splices word clips from
a speech corpus into YouTube Poop videos and YTPMV music, behind a multi-user
web UI (SvelteKit, embedded in the binary).

Phase 1: server foundation, accounts/roles/tokens, corpus install/list/delete.
Phase 2 (this): text and splice logic ported from the Python app, ffmpeg rendering, a job
queue, and `POST /api/ytp/generate` with the `/ytp` page working end to end.
YTPMV and projects still answer 501. API contract: [`docs/api.md`](docs/api.md).

Rendering needs `ffmpeg` on `PATH` (tested with 8.0.1), or `--ffmpeg <path>`.
Not ported yet: singing/vocoder, forced alignment (cuts use the corpus's stored phones),
per-corpus `pronunciations.csv`, the ffmpeg clip cache.

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

## Test

```sh
cargo test
cargo clippy --all-targets
```
