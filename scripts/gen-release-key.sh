#!/usr/bin/env bash
# Generates the ed25519 release-signing keypair (run by the maintainer, once).
#   - private key: piped straight into the repo secret ULAK_RELEASE_SIGNING_KEY (never printed, then shredded)
#   - public key:  crates/ulakd/release-key.pub, ENCODING = the raw 32-byte ed25519 public key as
#                  64 lowercase hex chars plus a trailing newline (no PEM, no prefix).
# Needs: openssl >= 1.1.1, gh (authenticated, admin on the repo), xxd, shred. Idempotent: if a key
# or secret already exists it asks before replacing both. Env: REPO=owner/name to override the repo.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
pub_file="$root/crates/ulakd/release-key.pub"
secret=ULAK_RELEASE_SIGNING_KEY
repo_args=()
[ -n "${REPO:-}" ] && repo_args=(--repo "$REPO")

for tool in openssl gh xxd shred; do
  command -v "$tool" >/dev/null || { echo "missing tool: $tool" >&2; exit 1; }
done

have_key=no
if [ -f "$pub_file" ] && grep -qE '^[0-9a-f]{64}$' "$pub_file"; then have_key=yes; fi
have_secret=no
if gh secret list "${repo_args[@]}" 2>/dev/null | awk '{print $1}' | grep -qx "$secret"; then have_secret=yes; fi

if [ "$have_key" = yes ] || [ "$have_secret" = yes ]; then
  echo "A release key already exists (public key file: $have_key, repo secret: $have_secret)."
  echo "Replacing it means every already-installed ulakd that embeds the old public key will"
  echo "REFUSE updates signed with the new key until it is reinstalled by hand."
  read -r -p "Type 'replace' to continue: " answer
  [ "$answer" = replace ] || { echo "aborted, nothing changed"; exit 1; }
fi

tmp=$(mktemp -d)
cleanup() {
  [ -f "$tmp/key.pem" ] && shred -u "$tmp/key.pem"
  rm -rf "$tmp"
}
trap cleanup EXIT
umask 077

openssl genpkey -algorithm ed25519 -out "$tmp/key.pem"
openssl pkey -in "$tmp/key.pem" -pubout -out "$tmp/pub.pem"
pub_hex=$(openssl pkey -in "$tmp/key.pem" -pubout -outform DER | tail -c 32 | xxd -p -c 64)
[[ "$pub_hex" =~ ^[0-9a-f]{64}$ ]] || { echo "could not extract the raw public key" >&2; exit 1; }

# Sign/verify round trip, exactly as release.yml signs.
printf 'ulak release key self-test' > "$tmp/msg"
openssl pkeyutl -sign -rawin -inkey "$tmp/key.pem" -in "$tmp/msg" -out "$tmp/msg.sig"
openssl pkeyutl -verify -rawin -pubin -inkey "$tmp/pub.pem" -in "$tmp/msg" -sigfile "$tmp/msg.sig" >/dev/null
echo "sign/verify round trip: ok"

# The secret first: if this fails no public key is written, so the two never disagree.
gh secret set "$secret" "${repo_args[@]}" < "$tmp/key.pem"
echo "repo secret $secret set"

printf '%s\n' "$pub_hex" > "$pub_file"
echo "public key written to $pub_file"
echo "  $pub_hex"
echo "Commit crates/ulakd/release-key.pub; the private key is gone from this machine."
