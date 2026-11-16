# Running ulakd as a service

Linux x86_64 with systemd. Nothing here has been run on a real machine yet.

## Install

```sh
sudo useradd --system --create-home --home-dir /home/ulakd --shell /usr/sbin/nologin ulakd
sudo apt-get install -y tmux
# Download from the GitHub release: ulakd-x86_64-unknown-linux-gnu.tar.gz, SHA256SUMS (+ .sig files)
sha256sum -c --ignore-missing SHA256SUMS
sudo install -d -o ulakd -g ulakd -m 0700 /var/lib/ulakd /var/lib/ulakd/bin /var/lib/ulakd/config
tar -xzf ulakd-x86_64-unknown-linux-gnu.tar.gz
sudo install -o ulakd -g ulakd -m 0755 ulakd /var/lib/ulakd/bin/ulakd
sudo cp deploy/ulakd.service /etc/systemd/system/
sudo -u ulakd ULAKD_DIR=/var/lib/ulakd/config /var/lib/ulakd/bin/ulakd join https://HUB:8443   # asks for the hub's one-time code
sudo systemctl enable --now ulakd
```

The binary sits in `/var/lib/ulakd/bin` (owned by `ulakd`), not `/usr/local/bin`, so the updater can replace it without root.
Config and certificates live in `ULAKD_DIR=/var/lib/ulakd/config`.

## Updating

The UI can ask a node to check for and install a newer release. The daemon downloads the release assets
from its own configured repo, checks `SHA256SUMS`, verifies the ed25519 signatures against the public key
compiled into it (`crates/ulakd/release-key.pub`), swaps the binary atomically and exits; systemd
(`Restart=always`) starts the new one. If there is no valid public key, or any check fails, nothing is
installed and the node reports `Failed` with the reason. The previous binary stays until the swap succeeds.

`ulakd` exits with code 75 after installing; `Restart=always` (or `--restart=always` in docker) starts the new one.
`KillMode=process` in the unit keeps the tmux agents alive across that restart.

Config: `update.json` in `ULAKD_DIR` (written with defaults on first run):

| field | default | meaning |
|---|---|---|
| `repo` | `ensaremirerol/ulak` | `owner/name` publishing the releases |
| `api_base` | `https://api.github.com` | API root (https only in normal builds) |
| `token_file` | none | file whose first line is a GitHub token (private repo); `ULAKD_GH_TOKEN` wins. Sent only to `api_base`'s origin, never on redirects |
| `interval_hours` | 6 | auto-check period |
| `auto_update` | `true`; `false` inside a container | install new releases without asking. `ULAKD_AUTO_UPDATE=0/1` overrides the file |

Manual: `ulakd update --check` / `ulakd update --apply`. Only strictly newer plain `vX.Y.Z` releases are installed.

Rollback: before swapping, the old binary is kept as `ulakd.prev` and a marker `update-pending.json` is written.
If the new binary dies before 30 s of uptime, the next start restores `ulakd.prev`, records the version in
`update-skip` (the auto loop will not retry it; a UI-triggered update still can) and exits 75.

Containers: the image ships with `ULAKD_AUTO_UPDATE=0` and the binary in `/opt/ulakd/bin` (writable by `ulakd`).
Prefer pulling a new image. To self-update anyway, run with `--restart=always` and `-e ULAKD_AUTO_UPDATE=1`,
or trigger it from the UI / `ulakd update --apply`. The swap is lost if the container is recreated.

Test: `scripts/test-updater-docker.sh` (needs docker; uses a throwaway key and a fake GitHub).

## Signing key (maintainer, once)

```sh
scripts/gen-release-key.sh      # needs gh (logged in, admin on the repo), openssl, xxd, shred
git add crates/ulakd/release-key.pub && git commit
```

It generates an ed25519 key, stores the private half only in the repo secret `ULAK_RELEASE_SIGNING_KEY`,
and writes the public half to `crates/ulakd/release-key.pub` as the raw 32-byte key in 64 hex chars.
Until it has run, the file holds a placeholder and daemons built from it refuse every update. Re-running
asks for confirmation: replacing the key makes already-installed daemons reject releases signed with the
new one.

Cut a release by tagging: bump the version in all crates, `git tag vX.Y.Z && git push --tags`
(`release.yml` fails if the tag differs from the crate version). `workflow_dispatch` with `dry_run` builds and signs only.
