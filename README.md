# ulak

Remote control for AI coding agents. A hub relays between node daemons (which run agents in tmux on your machines) and UIs (a native Rune app and a browser build). You see all agents, attach to their terminals, answer their questions (MCP `ask_human`), spawn and kill them, and read a mailbox, from anywhere.

## Architecture

- `crates/ulak-proto`: wire types (JSON frames) shared by all parties.
- `crates/ulak-remote`: the hub. Private CA, mTLS for nodes and native UIs, SQLite store (devices, passkeys, sessions, mail), registry, scheduler, MCP endpoint for agents, static serving of the web UI, WebAuthn.
- `crates/ulakd`: node daemon. Enrols with the hub, runs agents in tmux, streams screens, reports load/busy state (gaming/idle), reconnects with backoff.
- `crates/ulak`: the UI (Rune). Native binary `ulak` and a wasm build for browsers (`scripts/build-web.sh`).
- Native UIs and nodes authenticate with client certificates issued by the hub CA after enrolment; browsers use passkeys (WebAuthn) and a session (HttpOnly cookie or Bearer token).
- Certificates last 90 days and are renewed over mTLS (`POST /renew`) inside the last 30 days.
- Agents never see hub credentials: the hub hands each spawned agent a per-agent `ULAK_AGENT_TOKEN` for its MCP calls.
- API details: `crates/ulak-remote/README.md`; progress and decisions: `PROGRESS.md`.

## Quick start

Needs Rust (stable), tmux on the node machines. Browser build also needs the wasm target and CLI:
`rustup target add wasm32-unknown-unknown` and `cargo install wasm-bindgen-cli --version <wasm-bindgen in Cargo.lock> --locked`.

```sh
# 1. Hub (prints a one-time code and the CA fingerprint)
ULAK_BOOTSTRAP_OTP=node ULAK_SANS=hub.example.org,localhost cargo run -p ulak-remote

# 2. Node daemon (enrol once with the code + fingerprint, then run)
cargo run -p ulakd -- join https://hub.example.org:8443
cargo run -p ulakd -- run

# 3. Native UI (hub: stdin `otp ui`, or Devices > Add device)
cargo run -p ulak -- join https://hub.example.org:8443
cargo run -p ulak

# 4. Web UI
scripts/build-web.sh                        # -> target/web
ULAK_WEB_DIR=$PWD/target/web ULAK_RP_ID=hub.example.org ULAK_ORIGIN=https://hub.example.org:8443 cargo run -p ulak-remote
```

Browser on localhost without TLS (development only): add `ULAK_DEV_HTTP=1` to the hub and open `http://localhost:8080`. The Secure cookie is not kept on http, so after login use `/?token=<session token>` to survive a reload.

## Enrollment

1. The hub creates a one-time code (OTP: 8 characters, 5 minutes, single use, locks after 3 wrong tries) for a kind: `node`, `ui` or `browser`. Sources: `ULAK_BOOTSTRAP_OTP`, stdin `otp <kind>`, or Devices > Add device in the UI.
2. The new device is given the hub CA fingerprint out of band (printed next to the OTP) and checks that the CA the hub presents matches it (trust on first use, pinned by fingerprint).
3. Node/UI: generates a key, sends OTP + public key, gets a client cert; later connections use mTLS.
4. Browser: enter the OTP on the login screen to register a passkey; later "Sign in with passkey" gives a 7-day session. Revoking a device or passkey drops its connections.

## Environment variables

| Variable | Used by | Meaning |
|---|---|---|
| `ULAK_DIR` | hub, ulak | hub data dir (default `./ulak-data`); for `ulak`, its config dir (default `$XDG_CONFIG_HOME/ulak` or `~/.config/ulak`) |
| `ULAKD_DIR` | ulakd | daemon config dir (default `$XDG_CONFIG_HOME/ulakd` or `~/.config/ulakd`) |
| `ULAK_ADDR` | hub | listen address (default `0.0.0.0:8443`) |
| `ULAK_SANS` | hub | server certificate names (default `localhost,127.0.0.1`) |
| `ULAK_MCP_URL` | hub | MCP URL handed to agents |
| `ULAK_RP_ID`, `ULAK_ORIGIN` | hub | WebAuthn relying party id and origin (default: first non-IP SAN) |
| `ULAK_HTTP_ADDR` | hub | optional plain-HTTP listener for a TLS-terminating reverse proxy, e.g. `0.0.0.0:8080` (off when unset); see [the hub README](crates/ulak-remote/README.md#behind-a-reverse-proxy) |
| `ULAK_TRUSTED_PROXIES` | hub | comma list of proxy IPs/CIDRs whose `X-Forwarded-For` is believed (default none) |
| `ULAK_WEB_DIR` | hub | folder with the web build, served at `/` |
| `ULAK_BOOTSTRAP_OTP` | hub | `node`, `ui` or `browser`: print a one-time code at start |
| `ULAK_DEV_HTTP`, `ULAK_DEV_HTTP_PORT` | hub | `1` = also plain HTTP on 127.0.0.1 (default port 8080); rp_id `localhost` |
| `ULAK_ASK_TIMEOUT_MS`, `ULAK_NUDGE_TICK_MS`, `ULAK_NUDGE_EVERY_MS` | hub | question timeout and nudge timing (test knobs) |
| `ULAK_CERT_DAYS`, `ULAK_RENEW_WINDOW_DAYS` | hub | cert lifetime (90) and renewal window (30) (test knobs) |
| `ULAKD_OTP`, `ULAKD_FINGERPRINT` | ulakd join | answer the prompts non-interactively |
| `ULAK_OTP`, `ULAK_FINGERPRINT` | ulak join | same for the native UI |
| `ULAK_AGENT_TOKEN` | agents | set by the hub for each spawned agent (do not set yourself) |

## Docker images

`ghcr.io/<owner>/ulak-hub` and `ghcr.io/<owner>/ulakd` are published by `.github/workflows/docker.yml` for `linux/amd64` and `linux/arm64` (tags `latest`, the version, the commit sha). The hub image has the web build in `/app/web` (the default `ULAK_WEB_DIR`), keeps its state in the volume `/data`, and exposes 8443 (mTLS and web) and 8080 (plain HTTP, only listening when `ULAK_HTTP_ADDR=0.0.0.0:8080` is set). Reverse-proxy setup: [hub README](crates/ulak-remote/README.md#behind-a-reverse-proxy).

```
docker run -d -v ulak-data:/data -p 8443:8443 -e ULAK_SANS=hub.example.org ghcr.io/<owner>/ulak-hub
```

## Blockers

If an agent comes back asking the user to log in (or cannot be installed on a node), the hub raises a permanent blocker for that agent kind on that node. It shows in the UI until you mark it resolved, and no agent of that kind starts on that node meanwhile; automatic placement picks another node. Other nodes and other agent kinds are unaffected.

## Tests

```sh
cargo test --workspace        # tmux-based tests skip when tmux is missing
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
scripts/build-web.sh          # wasm build; open the result via the hub (see above)
```

## Known limits

- Terminal size is fixed by the daemon; the UI cannot request a resize.
- No native iOS app; phones use the browser build (WebGPU required).
- Web build: no logout button (API exists), no session renewal, the token is kept in memory only.
- Hub: SQLite called synchronously in handlers, static files have no gzip/Range, challenges are lost on restart.
- Everything is one hub, one user: no accounts or per-device permissions.

## Platforms and release assets

| Component | Platforms |
|---|---|
| Hub (`ulak-remote`) | Linux, Docker (amd64, arm64) |
| Daemon (`ulakd`) | Linux only (needs tmux and `/proc`); on Windows run it inside WSL |
| UI (`ulak`) | Linux, macOS (Apple Silicon and Intel), Windows 10+ (x86_64), and the browser build |

A `v*` tag publishes, each with a detached ed25519 `.sig` plus a signed `SHA256SUMS`: `ulakd-`, `ulak-remote-` and `ulak-x86_64-unknown-linux-gnu.tar.gz`; `ulak-aarch64-apple-darwin.tar.gz`; `ulak-x86_64-apple-darwin.tar.gz`; `ulak-x86_64-pc-windows-msvc.zip`.

The UI keeps its enrolment in `$ULAK_DIR`, else `%APPDATA%\ulak` (Windows), `~/Library/Application Support/ulak` (macOS), `$XDG_CONFIG_HOME/ulak` or `~/.config/ulak` (Linux). The key file is mode 0600 on Unix; on Windows it relies on the ACL of the per-user `%APPDATA%` directory (no extra hardening).

The binaries are not code-signed or notarized. macOS Gatekeeper quarantines downloaded files: run `xattr -d com.apple.quarantine ulak` once. Windows SmartScreen warns on first run ("More info", "Run anyway"). Verify downloads against the signed `SHA256SUMS`.

## Versions, updates, releases
Every binary reports its version (`ulak-remote --version`; hub `GET /api/version`, UI socket `HubVersion`/`NodeVersion`). The UI can ask nodes to check for and install updates (`UiToHub::CheckUpdate`/`UpdateNode`). Releases are built, signed (ed25519) and published by `.github/workflows/release.yml` on a `v*` tag; one-time key setup is `scripts/gen-release-key.sh`; service install and update behaviour are in `deploy/README.md`. Contract details: PROGRESS.md, "Versions and updates".

## Agent install and login blockers (ulakd)
`ulakd` reads `agents.json` from its config dir (created with defaults on first run). Per agent binary: a `check`, an optional `install` argv (run without a shell, only from this local file), and `login_patterns`. A missing agent is installed before it is spawned; if that fails, or the agent asks the user to log in, the hub shows a permanent blocker, further spawns of that profile on that node are refused, and the tmux session stays open so you can attach and log in. Resolve the blocker in the UI when done.
