# ulak-remote (hub)

HTTPS hub: mTLS WebSockets for daemons and UIs, enrollment, MCP, passkey login for browsers, static web build.

## Run
```
ULAK_BOOTSTRAP_OTP=ui ULAK_WEB_DIR=./target/web ULAK_SANS=hub.example.org,127.0.0.1 cargo run -p ulak-remote
```
Env: `ULAK_DIR` (data, default `./ulak-data`: `ca.pem`, `ca.key`, `hub.db`), `ULAK_ADDR` (default `0.0.0.0:8443`), `ULAK_SANS` (server cert names, default `localhost,127.0.0.1`), `ULAK_MCP_URL`, `ULAK_RP_ID` / `ULAK_ORIGIN` (passkey relying party; default first non-IP SAN, WebAuthn needs a domain), `ULAK_HTTP_ADDR` (optional plain-HTTP listener for a reverse proxy, e.g. `0.0.0.0:8080`; off when unset), `ULAK_TRUSTED_PROXIES` (comma list of proxy IPs/CIDRs whose `X-Forwarded-For` is believed; default empty = trust nobody), `ULAK_WEB_DIR` (web build served at `/`; GET/HEAD, ETag, SPA fallback, symlinks leaving the dir are refused), `ULAK_MAX_CPU_PCT` / `ULAK_MAX_GPU_PCT` (optional hub-side caps on a node's reported CPU/GPU percent for spawning; default 100 = off; the node's own state, judged per core by ulakd, decides, and the hub does not look at load average), `ULAK_BOOTSTRAP_OTP=node|ui|browser` (print a one-time code at start).

## First-time setup
1. Start the hub (Docker: the compose snippet below). It prints its listen address and creates `$ULAK_DIR/admin.sock`.
2. Mint a code for your first UI on the running hub, no restart needed:
   `docker compose exec hub ulak-remote admin otp ui` (or `docker exec <container> ulak-remote admin otp ui`; without Docker: `ulak-remote admin otp ui`, with the same `ULAK_DIR` or `--dir`).
   ```sh
   docker compose exec hub ulak-remote admin otp ui
   docker exec ulak-hub ulak-remote admin otp ui          # plain docker; ulak-hub = your container name
   docker compose exec hub ulak-remote admin fingerprint   # CA sha256 (lowercase hex), as the hub logs it at start
   docker compose exec hub ulak-remote admin ca --pem      # CA certificate (`admin ca` prints its path)
   ```
   It prints `OTP <code> (hub CA sha256 <fingerprint>)`, the expiry in seconds and, when `ULAK_SANS` is set in that environment, a ready line `ulak join https://<first SAN>`.
3. On your machine: `ulak join https://<hub>` and enter the code and fingerprint.
4. More machines: UI "Add device", or `admin otp node` / `admin otp browser`. List and revoke with `admin devices`, `admin revoke <id>`, `admin passkeys`, `admin revoke-passkey <id>`; `admin status` shows version, listen addresses and counts.
5. Lost every UI device: run `admin otp ui` again. Access to the admin socket (the data dir, i.e. `docker exec`) is the recovery credential, so keep the host private.

The admin socket is a Unix socket, mode 0600, so only processes that can open `$ULAK_DIR/admin.sock` can use it (the hub's user, root, or `docker exec`). It is removed on clean exit and replaced if stale after a crash. The hub refuses a path that exists and is not a socket, and a world-writable data dir. Requests are one JSON line (max 4 KiB, 5 s read timeout, `{"v":1,"op":"otp","kind":"ui"}`); admin calls are not enrollment attempts and do not count toward the lockout. Linux/macOS only. The stdin commands (`otp`, `revoke`) still work.

```yaml
services:
  hub:
    image: ghcr.io/<owner>/ulak-hub
    restart: unless-stopped
    ports: ["8443:8443"]
    environment:
      ULAK_SANS: hub.example.org
    volumes:
      - ulak-data:/data   # CA, hub.db and admin.sock; user ulak (uid 10001) owns /data
volumes:
  ulak-data:
```
The image's entrypoint is `ulak-remote`, and the binary is on the PATH, so the `exec` commands above work as shown.

## Docker image
`ghcr.io/<owner>/ulak-hub`, multi-arch (`linux/amd64`, `linux/arm64`), built from `Dockerfile.hub` (Rust is cross-compiled on the build host, not emulated).
- `/app/web`: the browser UI (Preact PWA from `web/`). This is the value for `ULAK_WEB_DIR`, already set in the image; mount your own folder there to override. Outside Docker build it with `scripts/build-web.sh --out <dir>` (needs Node 22).
- Web development: run a hub with `ULAK_HTTP_ADDR=127.0.0.1:8080`, then `cd web && npm run dev`; Vite proxies `/api`, `/auth` and `/ws` (websocket) to `http://localhost:8080`.
- PWA install: the browser only offers install (and passkeys/service workers only work) on HTTPS or `localhost`. Behind a reverse proxy set `ULAK_ORIGIN` (public `https://...` origin) and `ULAK_RP_ID` (its domain) to match the URL users open. The old Rune/wasm web bundle no longer exists.
- `/data` (volume, user `ulak`): `ULAK_DIR` with `ca.pem`, `ca.key`, `hub.db`. Keep it, the CA lives here.
- Ports: 8443 (mTLS and web; daemons and native UIs must reach this one), 8080 (plain HTTP, only when `ULAK_HTTP_ADDR=0.0.0.0:8080` is set).

## Behind a reverse proxy
Set `ULAK_HTTP_ADDR` and let the proxy terminate TLS. The plain listener serves the same routes as 8443 except the mTLS-only ones: `/ws/daemon` and `/renew` answer 404, and `/ws/ui` and `/api/*` need a session (cookie or bearer; there is no client cert here). `/enroll`, `/mcp`, `/auth/*` and the web build behave as on 8443. Daemons and native UIs keep connecting to 8443 directly (TCP passthrough or an open port), since mTLS cannot go through an HTTP proxy.

Required settings, because the hub cannot see the public URL:
- `ULAK_RP_ID=hub.example.org` and `ULAK_ORIGIN=https://hub.example.org` (exactly what the browser shows, no path, no trailing slash; add `:port` if not 443). These are the only source of the passkey relying party and of the `Origin` that `/ws/ui` accepts. A browser WebSocket upgrade with a different `Origin` gets 403; clients that send no `Origin` are unaffected. Changing them invalidates existing passkeys.
- `ULAK_TRUSTED_PROXIES=<proxy ip or cidr>` (e.g. `172.18.0.0/16` for a docker network, `127.0.0.1`). The enrollment rate limit is per client IP. The client IP is the socket peer, unless the peer is trusted; then `X-Forwarded-For` is read from the right, skipping trusted addresses, and the first untrusted one wins (the leftmost entry is client-controlled and never used). A malformed header falls back to the peer. With the list empty every request looks like the proxy and shares one limit; the hub logs a warning at start when `ULAK_HTTP_ADDR` is non-loopback and the list is empty. Never trust an address that clients can reach directly.
- `ULAK_MCP_URL`: the URL agents call for `/mcp`; the default is the 8443 address, so set it to the public URL if agents should use the proxy.
- The session cookie is always `Secure; HttpOnly; SameSite=Strict` because the browser sees HTTPS; plain `http://` access to 8080 will not keep a login.
- Keep 8080 off the public network (bind to a docker network or `127.0.0.1`), only the proxy should reach it.

nginx:
```
server {
    listen 443 ssl;
    server_name hub.example.org;
    # ssl_certificate / ssl_certificate_key ...
    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_read_timeout 1h;
    }
}
```
`$proxy_add_x_forwarded_for` appends the peer to what the client sent, which is what the right-to-left walk expects.

Caddy (sets `X-Forwarded-For` and `X-Forwarded-Proto` and upgrades WebSockets by itself):
```
hub.example.org {
    reverse_proxy 127.0.0.1:8080 {
        header_up X-Forwarded-Proto {scheme}
    }
}
```
Caddy trusts no upstream `X-Forwarded-For` by default and replaces it with the peer address, so that case is safe too.

Run with the image:
```
docker run -d -v ulak-data:/data -p 8443:8443 -p 127.0.0.1:8080:8080 \
  -e ULAK_HTTP_ADDR=0.0.0.0:8080 -e ULAK_TRUSTED_PROXIES=172.17.0.1 \
  -e ULAK_SANS=hub.example.org -e ULAK_RP_ID=hub.example.org -e ULAK_ORIGIN=https://hub.example.org \
  ghcr.io/<owner>/ulak-hub
```
(With a published port the proxy on the host reaches the container as the docker gateway, e.g. `172.17.0.1`; check `docker network inspect bridge`.)

## Enrolling
A one-time code (8 chars, 5 min, single use, 3 wrong codes lock until a new one is created) plus the CA fingerprint: from `ULAK_BOOTSTRAP_OTP`, stdin `otp device|browser` (`node`/`ui` are aliases of `device`), the UI "Add device", or `POST /api/devices/otp`. Nodes and UIs run `ulakd join` / `ulak join`; browsers register a passkey with a `browser` code.

## Security model
Any enrolled (non-revoked) device may act as a node (`/ws/daemon`) or as a UI (`/ws/ui`, `/api/*`); the recorded kind (node/ui) is for display only, and one `device` code works for both `ulakd join` and `ulak join`. Browsers are separate: passkey and session only, and a session cannot open `/ws/daemon`. The catch: agents run as your user, so a stolen or compromised node key can control the hub (spawn, kill, answer, add devices) until that device is revoked. Mitigation: revoke it from Devices or `ulak-remote admin revoke <id>`. If you later add less-trusted machines, per-device capability flags (node only, UI only) are the way to restore the separation.

## Admin (stdin)
Prefer the admin socket (`ulak-remote admin ...`, see First-time setup); stdin is only useful with an attached terminal.
`otp device|browser`, `revoke <device-id>` (closes its sockets; a revoked node leaves the registry and UIs see it Offline).

## Endpoints
- mTLS (8443 only): `/ws/daemon`, `/ws/ui` (also session cookie/token), `POST /renew` (`{"public_key": PEM}`, only in the last 30 days of the cert; 409 `{"earliest_renewal_ms"}` before; 403 revoked).
- No client cert: `POST /enroll`, `POST /mcp` (agent bearer token), `/auth/register/{start,finish}`, `/auth/login/{start,finish}`, `POST /auth/logout` (session only, 204).
- Admin (session or UI cert): `GET /api/devices`, `POST /api/devices/otp`, `POST /api/devices/{id}/revoke`, `GET /api/passkeys`, `POST /api/passkeys/{id}/revoke`.

`HubConfig` also has `cert_days` (90) and `renew_window_days` (30) for tests/tuning.

## Blockers

A blocker is a permanent problem for one agent profile on one node (`Login`: the agent asked the user to log in; `Install`: the agent binary is missing and ulakd could not install it). Stored in the `blockers` table; never expires, is never nudged, survives restarts, and is only closed by the user (`UiToHub::ResolveBlocker`).

- `/ws/daemon` `AgentBlocked{agent, profile, kind, detail}` is recorded under the daemon's own device id; a repeat for the same node+profile+kind only updates the detail.
- `/ws/ui` sends `Snapshot`, then `Blockers{blockers}`; later `BlockerOpened` / `BlockerClosed{id}`.
- `Spawn` for a blocked (node, profile) is refused with `Error`; `SpawnTarget::Auto` skips blocked nodes (`pick_node_where`).
- Resolving sends `BlockerResolved{profile}` to the node and the constant nudge "Login finished. Please continue." to the blocked agent if connected.
