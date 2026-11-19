# ulak

Remote control for AI coding agents. A hub relays between node daemons (which run agents in tmux on your machines) and UIs (a native Rune app and a browser build). You see all agents, attach to their terminals, answer their questions (MCP `ask_human`), spawn and kill them, and read a mailbox, from anywhere.

## Architecture

- `crates/ulak-proto`: wire types (JSON frames) shared by all parties.
- `crates/ulak-remote`: the hub. Private CA, mTLS for nodes and native UIs, SQLite store (devices, passkeys, sessions, mail), registry, scheduler, MCP endpoint for agents, static serving of the web UI, WebAuthn.
- `crates/ulakd`: node daemon. Enrols with the hub, runs agents in tmux, streams screens, reports load/busy state (gaming/idle), reconnects with backoff.
- `crates/ulak`: the UI (Rune). Native binary `ulak` only; the old wasm browser build is gone.
- `web/`: the browser UI, a Preact PWA (passkey login, installable). Built by `scripts/build-web.sh`.
- Native UIs and nodes authenticate with client certificates issued by the hub CA after enrolment; browsers use passkeys (WebAuthn) and a session (HttpOnly cookie or Bearer token).
- Certificates last 90 days and are renewed over mTLS (`POST /renew`) inside the last 30 days.
- Agents never see hub credentials: the hub hands each spawned agent a per-agent `ULAK_AGENT_TOKEN` for its MCP calls.
- API details: `crates/ulak-remote/README.md`; progress and decisions: `PROGRESS.md`.

## First-time setup

Start the hub, then mint the first UI code on the running hub (no restart, no stdin):

```sh
docker compose exec hub ulak-remote admin otp ui     # or: docker exec <container> ulak-remote admin otp ui
ulak join https://hub.example.org:8443               # on your machine, with that code and fingerprint
```

More machines: Devices > Add device, or `admin otp node`. Lost all UI devices: run `admin otp ui` again. Also `admin devices|revoke|passkeys|revoke-passkey|status`. The command talks to `$ULAK_DIR/admin.sock` (0600, local only). Compose example and details: [hub README](crates/ulak-remote/README.md#first-time-setup).

## Add a node in one command

On the node (Linux x86_64 or arm64, glibc 2.34+, tmux; Windows: inside WSL with systemd on), as the user the agents should run as:

```sh
curl -fsSL https://github.com/ensaremirerol/ulak/releases/latest/download/install-ulakd.sh | sh -s -- https://hub.example.org
```

It downloads the latest `ulakd`, checks its ed25519 signature and checksum, installs it to `~/.local/share/ulak/bin`, joins the hub (asks for the one-time code and the hub fingerprint; or pass `ULAKD_OTP=... ULAKD_FINGERPRINT=...` before `sh`) and runs it as the systemd user service `ulakd`. No sudo; `sudo loginctl enable-linger $USER` keeps it running after logout. Re-running it upgrades the binary. The web UI's Devices > Add device shows this command (and a Docker one) with the code filled in.

## Quick start

Needs Rust (stable), tmux on the node machines. The browser UI also needs Node.js 22 + npm.

```sh
# 1. Hub (prints a one-time code and the CA fingerprint)
ULAK_BOOTSTRAP_OTP=node ULAK_SANS=hub.example.org,localhost cargo run -p ulak-remote

# 2. Node daemon (enrol once with the code + fingerprint, then run)
cargo run -p ulakd -- join https://hub.example.org:8443
cargo run -p ulakd -- run

# 3. Native UI (hub: `ulak-remote admin otp ui`, or Devices > Add device)
cargo run -p ulak -- join https://hub.example.org:8443
cargo run -p ulak

# 4. Web UI
scripts/build-web.sh                        # npm ci + vite build -> target/web (--out DIR to change)
ULAK_WEB_DIR=$PWD/target/web ULAK_RP_ID=hub.example.org ULAK_ORIGIN=https://hub.example.org:8443 cargo run -p ulak-remote
```

Browser on localhost without TLS (development only): add `ULAK_DEV_HTTP=1` to the hub and open `http://localhost:8080`. The Secure cookie is not kept on http, so after login use `/?token=<session token>` to survive a reload.

## Enrollment

1. The hub creates a one-time code (OTP: 8 characters, 5 minutes, single use, locks after 3 wrong tries) for a kind: `device` (alias of `node`/`ui`: one code for `ulakd join` or `ulak join`) or `browser`. Sources: `ULAK_BOOTSTRAP_OTP`, stdin `otp <kind>`, or Devices > Add device in the UI.
2. The new device is given the hub CA fingerprint out of band (printed next to the OTP) and checks that the CA the hub presents matches it (trust on first use, pinned by fingerprint).
3. Device (node or UI): generates a key, sends OTP + public key, gets a client cert; later connections use mTLS.
4. Browser: enter the OTP on the login screen to register a passkey; later "Sign in with passkey" gives a 7-day session. Revoking a device or passkey drops its connections.

## Security model
Any enrolled (non-revoked) device may act as a node (`/ws/daemon`) or as a UI (`/ws/ui`, `/api/*`); the recorded kind (node/ui) is for display only, and one `device` code works for both `ulakd join` and `ulak join`. Browsers are separate: passkey and session only, and a session cannot open `/ws/daemon`. The catch: agents run as your user, so a stolen or compromised node key can control the hub (spawn, kill, answer, add devices) until that device is revoked. Mitigation: revoke it from Devices or `ulak-remote admin revoke <id>`. If you later add less-trusted machines, per-device capability flags (node only, UI only) are the way to restore the separation.

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
| `ULAK_GITHUB_API` | hub | GitHub REST base (default `https://api.github.com`; `http` only for loopback) |
| `ULAK_RELEASE_REPO`, `ULAK_RELEASE_INTERVAL_HOURS` | hub | repo (default `ensaremirerol/ulak`) and refresh interval in hours (6) of the node-update release cache |
| `ULAK_RELEASE_CREDENTIAL`, `ULAK_RELEASE_TOKEN_FILE`, `ULAK_GH_TOKEN` | hub | token for that fetch: stored credential id (preferred) or a file/env token kept in memory only. One token on the hub; nodes need none |
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
scripts/build-web.sh          # PWA build; open the result via the hub (see above)
(cd web && npm ci && npm run lint && npm run typecheck && npm test)
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

A `v*` tag publishes, each with a detached ed25519 `.sig` plus a signed `SHA256SUMS`: `ulakd-`, `ulak-remote-` and `ulak-x86_64-unknown-linux-gnu.tar.gz`; `ulakd-aarch64-unknown-linux-gnu.tar.gz`; `ulak-aarch64-apple-darwin.tar.gz`; `ulak-x86_64-apple-darwin.tar.gz`; `ulak-x86_64-pc-windows-msvc.tar.gz` (a `.zip` up to 0.1.5; Windows 10+ opens `.tar.gz` natively, or `tar -xzf` in a terminal). The hub image for a tag carries that release's `ulakd-*` and `ulak-*` archives (`/app/releases`, `ULAK_BUNDLED_RELEASES`), so nodes and apps update from the hub with no GitHub token.

### Windows node (WSL)
`ulakd` runs inside WSL as an ordinary Linux node and updates from the hub like one. In WSL (Ubuntu): turn on systemd (`/etc/wsl.conf`: `[boot]` `systemd=true`, then `wsl --shutdown` from Windows), `sudo apt install tmux`, unpack `ulakd-x86_64-unknown-linux-gnu.tar.gz` (or `aarch64` on Arm Windows) into `~/.local/share/ulak/bin`, `ulakd join https://<hub>`, and install the user service from `deploy/README.md`. WSL stops when no Windows session uses it; `loginctl enable-linger $USER` inside WSL keeps the user service running while WSL is up.

The UI keeps its enrolment in `$ULAK_DIR`, else `%APPDATA%\ulak` (Windows), `~/Library/Application Support/ulak` (macOS), `$XDG_CONFIG_HOME/ulak` or `~/.config/ulak` (Linux). The key file is mode 0600 on Unix; on Windows it relies on the ACL of the per-user `%APPDATA%` directory (no extra hardening).

The binaries are not code-signed or notarized. macOS Gatekeeper quarantines downloaded files: run `xattr -d com.apple.quarantine ulak` once. Windows SmartScreen warns on first run ("More info", "Run anyway"). Verify downloads against the signed `SHA256SUMS`.

## Versions, updates, releases
Every binary reports its version (`ulak-remote --version`; hub `GET /api/version`, UI socket `HubVersion`/`NodeVersion`). The UI can ask nodes to check for and install updates (`UiToHub::CheckUpdate`/`UpdateNode`). Releases are built, signed (ed25519) and published by `.github/workflows/release.yml` on a `v*` tag; one-time key setup is `scripts/gen-release-key.sh`; service install and update behaviour are in `deploy/README.md`. Hub side: the hub fetches and signature-verifies releases with its one GitHub token, caches the newest 2 and serves them to nodes over mTLS (`/api/releases/*`); nodes re-verify with their embedded key, so the hub is untrusted for integrity. Admin: `ulak-remote admin releases` / `releases-refresh`; see `crates/ulak-remote/README.md`. The native app updates itself the same way (Devices panel, "This app": Check, then Update app; it restarts with the same arguments, and a version that dies within 30 s is replaced by the previous one, `ulak.prev`, on the next start). 0.1.3 daemons only know GitHub, so 0.1.4 needs a one-time manual install. Contract details: PROGRESS.md, "Versions and updates".

### ulakd update source
`<config dir>/update.json` has `"source": "hub" | "github"`. Unset (the default) means `hub` when the daemon is enrolled (`hub_url`, `ca.pem`, `cert.pem`, `key.pem` present) and `github` otherwise; set `github` to keep fetching from GitHub (`repo`, `api_base`, token) even when enrolled. The hub source reads the identity files on every request (renewed certificates are picked up), trusts only the hub CA, presents the device certificate, never follows redirects, and fetches `/api/releases/latest` plus the archive, `.sig`, `SHA256SUMS` and `SHA256SUMS.sig`. The normal pipeline then runs unchanged (embedded-key signature, checksum, version newer than the running one, protocol check, atomic replace, exit 75). `ulakd update --check|--apply` and the UI check use the configured source. Failure messages: `hub has no release cached yet: ask the admin to run `ulak-remote admin releases-refresh``, `hub update source unreachable: <reason>` (network, TLS or identity problems), `hub refused the update request` (401/403).

## Agent install and login blockers (ulakd)
`ulakd` reads `agents.json` from its config dir (created with defaults on first run). Per agent binary: a `check`, an optional `install` argv (run without a shell, only from this local file), and `login_patterns`. A missing agent is installed before it is spawned; if that fails, or the agent asks the user to log in, the hub shows a permanent blocker, further spawns of that profile on that node are refused, and the tmux session stays open so you can attach and log in. Resolve the blocker in the UI when done.


## GitHub credentials and the spawn queue
- Credentials: the UI stores a GitHub token under a name (`add_credential`). The hub checks it against the GitHub API and keeps the login. Tokens are encrypted at rest (ChaCha20-Poly1305) with a random 32-byte key in `$ULAK_DIR/secrets.key` (mode 0600; the hub refuses a symlink or any group/other mode bit). **Back up `secrets.key` together with `hub.db`**: if it is lost, the stored tokens are unusable and must be re-added. The token is never sent to a UI, never logged and never printed by `admin credentials` (name, login, id only); `admin credential-delete <id>` removes one.
- The hub sends a token to a node only as `GitAccess` for the chosen node, immediately before the `Spawn` of a repo spawn.
- `ULAK_GITHUB_API` (default `https://api.github.com`) is the GitHub Enterprise hook: an `https` URL, or `http` only for a loopback host (tests, a local proxy).
- Queue: a spawn that cannot be placed now (no idle node, or the pinned node is busy or offline) is queued, FIFO, max 100 queued or failed items, one item per agent id. Items are placed in order when a node becomes idle (an `Auto` item takes the first idle node; a pinned item waits for its node). The queue is stored in `hub.db`, so it survives a hub restart. `cancel_queued` removes an item. An item whose placement fails (for example the credential was deleted) stays listed as failed with the reason until it is cancelled; it is never retried.

## Repo spawns (ulakd)
A spawn whose source is a GitHub repository (`owner/name`, optional branch) is cloned by the node itself, then the agent starts in the clone exactly like a folder spawn (MCP wiring, nudges, install and login blockers all apply).
- The hub sends `GitAccess{agent, token}` right before `Spawn`, only to the chosen node. ulakd keeps the token in memory only, single use, for 120 s; a repo spawn without it fails (`Failed`; the sanitized reason, at most 300 chars and never a credential, reaches the UI as an `agent_failed` frame and an error toast). The token is never logged and never written to disk.
- Clone: `git -c credential.helper= clone --depth 1 [--branch B] -- https://github.com/<owner>/<name>.git <dir>` as an argv vector (no shell, host fixed to github.com, repo and branch only from validated values). Directory: `<config>/work/<agent-id>/<repo-name>` (`ULAKD_WORK_DIR` moves the root; refused if it exists and is not empty). `ULAKD_CLONE_TIMEOUT_SECS` (default 300) kills the git process group. Clones are shallow: run `git fetch --unshallow` (or `--deepen N`) inside the clone for history. Clones are not deleted when the agent ends; a later spawn of the same repo reuses an unused one (see below).
- Credentials: a temporary 0700 askpass script (answers `Username` with `x-access-token`, `Password` with `$ULAK_GIT_TOKEN`) in a 0700 dir under the work root, with `GIT_ASKPASS`, `GIT_TERMINAL_PROMPT=0`, `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null`, `GCM_INTERACTIVE=never`, `SSH_ASKPASS` cleared. The token lives only in the git child's environment: not in argv, URL, `.git/config` or the process list. The helper is removed afterwards, also on failure or timeout. Failures map to readable messages (authentication failed, repository not found, branch not found, network, timeout) that never echo credentials.
- `allow_push: false` (default): the agent gets no token, and the clone gets `remote.origin.pushurl=no-push-here`. This is a guard rail, not a security boundary; the boundary is the scope of the token itself.
- `allow_push: true`: the daemon writes the token to `<config>/run/git-<agent>/token` (file mode 0600, dir 0700, created with `create_new`) and the agent gets `GIT_ASKPASS=<config>/run/git-<agent>/askpass` (0700, reads that file) and `GIT_TERMINAL_PROMPT=0`. The token is not in the `tmux new-session` argv, the process list or the agent's environment. Be honest about what this means: an agent with `allow_push` can read the token file and use the push credential; that is the point of the flag. The file is private to your user and is removed when the agent is killed or its session is gone, when a spawn fails, and in the sweep at daemon start.

## Profiles, repo index and cleanup (ulakd)
- Named profiles in `agents.json`: an entry may run another binary with its own config folder, e.g. two claude accounts:
  `{"claude-work":{"binary":"claude","config_dir":"~/.claude-work"},"claude-personal":{"binary":"claude","config_dir":"~/.claude-personal"}}`.
  A spawn picks one by using its name as the profile binary. Optional `args` and `env` per entry; `~` is expanded on the node; `config_dir` becomes `CLAUDE_CONFIG_DIR` for claude. Install/check/login settings left out come from the real binary's entry. Login blockers are per profile name, so the two accounts block independently. The node reports its profiles (installed, login blocked) to the hub.
- Installing an agent: give a profile an install script (web: Settings › Profiles, or a node's screen), e.g. `curl -fsSL https://example.org/install.sh | bash`. It runs on the node with `sh -c` when a spawn finds the program missing, or when you press Install / Reinstall on the node screen (or "Install on all nodes" for a global profile). Local `agents.json` recipes (e.g. claude's npm install) get the same buttons.
- Repo reuse: a repo spawn reuses an unused clone of the same repo under the work root (fetch, checkout of the branch, fast-forward only); a clone with uncommitted changes fails the spawn with a clear reason instead of being touched. `fresh: true` always clones anew.
- Repo index: the node reports its clones (branch, clean, ahead/behind, last fetch, in use) every `index_interval_mins` and on changes. `<config>/ulakd.json` (optional):
  `{"repo_roots": ["~/code"], "index_interval_mins": 10, "clean": {"auto": false, "after_exit_hours": 24}}`.
  `repo_roots` adds your own checkouts to the index; they are never reused or deleted.
- Cleanup (on request from the hub, or automatically with `clean.auto` after `after_exit_hours` without an agent): only `<work>/<agent>/<repo>` clones (canonical path, no symlinks), not in use, with no uncommitted/untracked files, no stash and no commit missing from the remote (as of the last fetch). Anything that cannot be verified is kept, with the reason in the report. A dry run only reports.
- Hub profiles and settings: the hub sends its profiles and this node's settings on connect and on every change; the daemon applies them live (no restart, running agents untouched) and does not store them, so after a restart the local files apply until the hub's next `Config` (sent right after reconnecting). A hub profile only sets `binary`, `args`, `config_dir` and `env`: with the name of a local `agents.json` entry it keeps that entry's check/install/login/MCP settings, a new name inherits them from its binary's local entry. Install and check commands always come from the node. Hub settings (`repo_roots`, `index_interval_mins`, `clean_auto`, `clean_after_exit_hours`, `auto_update`, `cpu_busy_pct`, `gpu_busy_pct`) override `ulakd.json`, `update.json` and `guard.json` one by one; an unset or out-of-range value falls back to the local one. Known limit: after a daemon restart, agents of hub-only profiles are re-adopted before the hub's profiles arrive, so their login prompt watching does not resume.

## How agents get connected to the hub (MCP)
When the hub spawns an agent it sets `ULAK_MCP_URL` and `ULAK_AGENT_TOKEN`. `ulakd` does the rest, per `agents.json` profile (binary file name):

- `"mcp": "claude"` (default for the binary `claude`): writes `<config dir>/run/mcp-<agent>.json` (mode 0600; it only refers to `${ULAK_AGENT_TOKEN}`, the token is not on disk), passes `--mcp-config <file>` (the user's own MCP servers keep working, no `--strict-mcp-config`) and sets `NODE_EXTRA_CA_CERTS=<config dir>/ca.pem` so the hub's own CA is trusted. The file is deleted when the agent is killed or its session ends, and stale files are removed when `ulakd` starts. The agent then has `send_message`, `read_inbox`, `ask_human`, `check_answer` (tools `mcp__ulak__*`).
- `"mcp": "none"` (default for every other binary, e.g. `antigravity`): nothing is added.
- Custom agents: set `"mcp_args": ["--mcp-server", "{mcp_url}", "--token-env", "{token_env}"]`; the argv is appended after the profile args. Placeholders: `{mcp_url}`, `{token_env}` (the name `ULAK_AGENT_TOKEN`; read the token from that env var), `{ca_file}` (hub CA pem). `NODE_EXTRA_CA_CERTS` is set too; a value in the spawn env wins.
