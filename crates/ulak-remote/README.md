# ulak-remote (hub)

HTTPS hub: mTLS WebSockets for daemons and UIs, enrollment, MCP, passkey login for browsers, static web build.

## Run
```
ULAK_BOOTSTRAP_OTP=ui ULAK_WEB_DIR=./web-build ULAK_SANS=hub.example.org,127.0.0.1 cargo run -p ulak-remote
```
Env: `ULAK_DIR` (data, default `./ulak-data`: `ca.pem`, `ca.key`, `hub.db`), `ULAK_ADDR` (default `0.0.0.0:8443`), `ULAK_SANS` (server cert names, default `localhost,127.0.0.1`), `ULAK_MCP_URL`, `ULAK_RP_ID` / `ULAK_ORIGIN` (passkey relying party; default first non-IP SAN, WebAuthn needs a domain), `ULAK_WEB_DIR` (web build served at `/`; GET/HEAD, ETag, SPA fallback, symlinks leaving the dir are refused), `ULAK_MAX_CPU_PCT` / `ULAK_MAX_GPU_PCT` (optional hub-side caps on a node's reported CPU/GPU percent for spawning; default 100 = off; the node's own state, judged per core by ulakd, decides, and the hub does not look at load average), `ULAK_BOOTSTRAP_OTP=node|ui|browser` (print a one-time code at start).

## Enrolling
A one-time code (8 chars, 5 min, single use, 3 wrong codes lock until a new one is created) plus the CA fingerprint: from `ULAK_BOOTSTRAP_OTP`, stdin `otp node|ui|browser`, the UI "Add device", or `POST /api/devices/otp`. Nodes and UIs run `ulakd join` / `ulak join`; browsers register a passkey with a `browser` code.

## Admin (stdin)
`otp node|ui|browser`, `revoke <device-id>` (closes its sockets; a revoked node leaves the registry and UIs see it Offline).

## Endpoints
- mTLS: `/ws/daemon`, `/ws/ui` (also session cookie/token), `POST /renew` (`{"public_key": PEM}`, only in the last 30 days of the cert; 409 `{"earliest_renewal_ms"}` before; 403 revoked).
- No client cert: `POST /enroll`, `POST /mcp` (agent bearer token), `/auth/register/{start,finish}`, `/auth/login/{start,finish}`, `POST /auth/logout` (session only, 204).
- Admin (session or UI cert): `GET /api/devices`, `POST /api/devices/otp`, `POST /api/devices/{id}/revoke`, `GET /api/passkeys`, `POST /api/passkeys/{id}/revoke`.

`HubConfig` also has `cert_days` (90) and `renew_window_days` (30) for tests/tuning.
