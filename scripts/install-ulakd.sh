#!/bin/sh
# One-command ulakd (node daemon) install for Linux x86_64 / aarch64, as the CURRENT user (no sudo):
#
#   curl -fsSL https://github.com/ensaremirerol/ulak/releases/latest/download/install-ulakd.sh | sh -s -- https://HUB[:PORT]
#
# Downloads the release archive, verifies its ed25519 signature (key below = crates/ulakd/release-key.pub)
# and checksum, installs the binary to $ULAK_PREFIX/bin (default ~/.local/share/ulak/bin, where the
# self-updater can replace it), joins the hub if this machine is not enrolled yet, and runs ulakd as a
# systemd user service. Re-running it upgrades the binary and keeps the enrolment.
#
# Env: ULAKD_OTP / ULAKD_FINGERPRINT answer the join prompts (else they are asked on the terminal);
#      ULAK_VERSION=0.1.7 installs that release instead of the latest; ULAK_BASE_URL a mirror of the
#      release files; ULAKD_DIR overrides the config dir.
# Windows: run it inside WSL (with systemd enabled). macOS: use the Docker image.
set -eu

REPO=ensaremirerol/ulak
PUBKEY='-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEAri1Bi64BZIybVNORf6xjS5Jw5IFD04ooPX8KEjpwnGI=
-----END PUBLIC KEY-----'

die() { echo "ulakd install: $*" >&2; exit 1; }
say() { echo "ulakd install: $*"; }

hub=${1:-}
[ "$(uname -s)" = Linux ] || die "Linux only (Windows: run this inside WSL; macOS: use the Docker image)"
case $(uname -m) in
  x86_64 | amd64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *) die "unsupported CPU $(uname -m) (x86_64 and aarch64 only)" ;;
esac
for t in curl tar sha256sum openssl; do
  command -v "$t" >/dev/null 2>&1 || die "needs $t (e.g. sudo apt-get install -y $t)"
done
case $hub in
  '' | https://*) ;;
  *) die "the hub address must start with https:// (got $hub)" ;;
esac

# glibc floor of the release binaries.
glibc=$(ldd --version 2>/dev/null | head -n1 | grep -o '[0-9]\+\.[0-9]\+$' || true)
if [ -n "$glibc" ] && [ "$(printf '%s\n' "$glibc" 2.34 | sort -V | head -n1)" != 2.34 ]; then
  die "glibc $glibc is too old (needs 2.34+: Debian 12, Ubuntu 22.04, Raspberry Pi OS bookworm or newer)"
fi
command -v tmux >/dev/null 2>&1 || say "WARNING: tmux is missing; agents cannot start until it is installed (sudo apt-get install -y tmux)"

if [ -n "${ULAK_BASE_URL:-}" ]; then
  base=$ULAK_BASE_URL # a mirror; the signature check below still applies
elif [ -n "${ULAK_VERSION:-}" ]; then
  base="https://github.com/$REPO/releases/download/v${ULAK_VERSION#v}"
else
  base="https://github.com/$REPO/releases/latest/download"
fi
tarball="ulakd-$arch-unknown-linux-gnu.tar.gz"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
say "downloading $tarball"
for f in "$tarball" "$tarball.sig" SHA256SUMS SHA256SUMS.sig; do
  curl -fsSL --retry 3 -o "$tmp/$f" "$base/$f" || die "download failed: $base/$f"
done

printf '%s\n' "$PUBKEY" >"$tmp/key.pem"
for f in SHA256SUMS "$tarball"; do
  openssl pkeyutl -verify -rawin -pubin -inkey "$tmp/key.pem" -in "$tmp/$f" -sigfile "$tmp/$f.sig" >/dev/null 2>&1 ||
    die "signature check failed for $f (or this openssl is older than 3.0); nothing was installed"
done
(cd "$tmp" && grep "  $tarball\$" SHA256SUMS | sha256sum -c --status) || die "checksum mismatch for $tarball"
[ "$(tar -tzf "$tmp/$tarball")" = ulakd ] || die "unexpected archive contents"
tar -xzf "$tmp/$tarball" -C "$tmp"

prefix=${ULAK_PREFIX:-$HOME/.local/share/ulak}
bin="$prefix/bin/ulakd"
mkdir -p "$prefix/bin"
install -m 0755 "$tmp/ulakd" "$bin.new"
mv -f "$bin.new" "$bin"
say "installed $("$bin" --version | head -n1) at $bin"

conf=${ULAKD_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/ulakd}
if [ -f "$conf/cert.pem" ]; then
  say "already enrolled ($conf)"
elif [ -n "$hub" ]; then
  say "joining $hub"
  # `curl | sh` makes the script our stdin: read prompts from the terminal unless env answers them.
  if [ -t 0 ] || { [ -n "${ULAKD_OTP:-}" ] && [ -n "${ULAKD_FINGERPRINT:-}" ]; }; then
    "$bin" join "$hub"
  elif [ -r /dev/tty ]; then
    "$bin" join "$hub" </dev/tty
  else
    die "no terminal for the join prompts: set ULAKD_OTP and ULAKD_FINGERPRINT"
  fi
else
  say "not enrolled yet: run '$bin join https://HUB' (or re-run this with the hub address), then this again"
  exit 0
fi

if command -v systemctl >/dev/null 2>&1 && systemctl --user show-environment >/dev/null 2>&1; then
  unit="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/ulakd.service"
  mkdir -p "$(dirname "$unit")"
  # PATH as it is now, so agents installed for this user (npm -g, ~/.local/bin) are found.
  cat >"$unit" <<EOF
[Unit]
Description=ulak node daemon
After=network-online.target

[Service]
ExecStart=$bin run
Environment=PATH=$PATH
Restart=always
RestartSec=5
# Agents live in tmux: a restart (or self-update, exit 75) must not kill them.
KillMode=process

[Install]
WantedBy=default.target
EOF
  systemctl --user daemon-reload
  systemctl --user enable ulakd.service >/dev/null 2>&1
  systemctl --user restart ulakd.service
  say "running as the systemd user service ulakd (logs: journalctl --user -u ulakd -f)"
  if [ "$(loginctl show-user "$(id -un)" -p Linger --value 2>/dev/null || echo no)" != yes ]; then
    say "to keep it running after you log out: sudo loginctl enable-linger $(id -un)"
  fi
else
  say "no systemd user session: start it yourself, e.g. nohup $bin run >\"$conf/ulakd.log\" 2>&1 &"
fi
