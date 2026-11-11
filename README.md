# ytp-machine

Rust rewrite of the "michael-rosen-says" YTP generator: splices word clips from
a speech corpus into YouTube Poop videos and YTPMV music, behind a multi-user
web UI (SvelteKit, embedded in the binary).

Phase 1 (this): server foundation, accounts/roles/tokens, corpus install/list/delete.
Generation endpoints answer 501 for now. API contract: [`docs/api.md`](docs/api.md).

## Layout

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
