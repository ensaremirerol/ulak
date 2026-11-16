#!/usr/bin/env bash
# End-to-end test of the ulakd self-updater against a real hub, in docker. Uses a throwaway signing
# key (never the real one), a fake GitHub, and removes everything it created on exit.
# Needs: docker, cargo, openssl, xxd. The images must be able to run host-built binaries (ubuntu:26.04).
set -uo pipefail
cd "$(dirname "$0")/.."
ROOT=$PWD
P=ulaktest$$
BASE_IMG=$P-base
NET=$P-net
HUB=$P-hub
T=$ROOT/target/updater-test/$$       # temp dir under the repo: /tmp has a quota
KT=$ROOT/target/testkey              # test-key binaries must not clobber target/debug/ulakd
FAILS=0; PIDS=()

cleanup() {
  exec 9>&- 2>/dev/null
  for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done
  docker ps -aq --filter "name=$P" | xargs -r docker rm -f >/dev/null 2>&1
  docker network rm "$NET" >/dev/null 2>&1
  docker volume ls -q --filter "name=$P" | xargs -r docker volume rm -f >/dev/null 2>&1
  docker image rm -f "$BASE_IMG" >/dev/null 2>&1
  rm -rf "$T"
}
trap cleanup EXIT
trap 'exit 130' INT TERM
mkdir -p "$T"/{out,good,bad,ui}

pass() { echo "PASS $1"; }
fail() { echo "FAIL $1: $2"; FAILS=$((FAILS + 1)); }
die() { echo "setup failed: $*" >&2; exit 2; }

echo "== keys and binaries"
openssl genpkey -algorithm ed25519 -out "$T/key.pem" 2>/dev/null || die openssl
PUB=$(openssl pkey -in "$T/key.pem" -pubout -outform DER | tail -c 32 | xxd -p -c 64)
for v in 0.0.1 99.0.0; do
  CARGO_TARGET_DIR=$KT ULAKD_TEST_RELEASE_KEY=$PUB ULAK_BUILD_VERSION=$v \
    cargo build -q -p ulakd --features test-key 2>&1 | tail -5
  cp "$KT/debug/ulakd" "$T/out/ulakd-$v" || die "build $v"
done
cargo build -q -p ulak-remote -p ulakd --examples 2>&1 | tail -5
D=$ROOT/target/debug
[ "$("$T/out/ulakd-99.0.0" --version | head -1 | cut -d' ' -f2)" = 99.0.0 ] || die "version override"

echo "== release assets (like release.yml)"
cp "$T/out/ulakd-99.0.0" "$T/good/ulakd"
tar -C "$T/good" -czf "$T/good/ulakd-x86_64-unknown-linux-gnu.tar.gz" ulakd
rm "$T/good/ulakd"
(cd "$T/good" && sha256sum ./*.tar.gz | sed 's# \./# #' > SHA256SUMS
 for f in ./*.tar.gz SHA256SUMS; do
   openssl pkeyutl -sign -rawin -inkey "$T/key.pem" -in "$f" -out "$f.sig" || exit 1
 done) || die sign
cp "$T"/good/* "$T/bad/"
printf 'x' >> "$T/bad/ulakd-x86_64-unknown-linux-gnu.tar.gz"   # signatures no longer match

echo "== docker base, network, hub"
docker build -q -t "$BASE_IMG" - >/dev/null <<'DF' || die "base image"
FROM ubuntu:26.04
RUN apt-get update && apt-get install -y --no-install-recommends tmux ca-certificates && rm -rf /var/lib/apt/lists/*
DF
docker network create "$NET" >/dev/null || die network
# fake GitHub in containers on the same network (host firewalls may block container -> host traffic)
GH=$P-gh; BAD=$P-ghbad
docker run -d --name "$GH" --network "$NET" -v "$D/examples/fake_github:/fg:ro" -v "$T/good:/assets:ro" \
  "$BASE_IMG" /fg 8000 "http://$GH:8000" /assets >/dev/null || die "fake github"
docker run -d --name "$BAD" --network "$NET" -v "$D/examples/fake_github:/fg:ro" -v "$T/bad:/assets:ro" \
  "$BASE_IMG" /fg 8000 "http://$BAD:8000" /assets >/dev/null || die "fake github (bad)"
mkfifo "$T/hub.in"; exec 9<>"$T/hub.in"
docker volume create "$P-hubdata" >/dev/null
docker run -i --name "$HUB" --network "$NET" -p 127.0.0.1::8443 -v "$P-hubdata:/data" \
  -v "$D/ulak-remote:/usr/local/bin/ulak-remote:ro" \
  -e ULAK_DIR=/data -e ULAK_ADDR=0.0.0.0:8443 -e ULAK_SANS="$HUB,127.0.0.1" \
  "$BASE_IMG" ulak-remote <&9 >"$T/hub.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 60); do grep -q 'listening on' "$T/hub.log" && break; sleep 0.5; done
grep -q 'listening on' "$T/hub.log" || { tail "$T/hub.log"; die "hub did not start"; }
HPORT=$(docker port "$HUB" 8443/tcp | head -1 | sed 's/.*://')
URL=https://127.0.0.1:$HPORT

# otp KIND -> sets OTP and FP from the hub log
otp() {
  local n; n=$(grep -c '^OTP ' "$T/hub.log")
  echo "otp $1" >&9
  for _ in $(seq 40); do [ "$(grep -c '^OTP ' "$T/hub.log")" -gt "$n" ] && break; sleep 0.25; done
  local l; l=$(grep '^OTP ' "$T/hub.log" | tail -1)
  OTP=$(cut -d' ' -f2 <<<"$l"); FP=${l##* }; FP=${FP%)}
}
otp ui
ULAKD_DIR=$T/ui ULAKD_OTP=$OTP ULAKD_FINGERPRINT=$FP "$T/out/ulakd-0.0.1" join "$URL" >/dev/null || die "ui join"

# new_node NAME GITHUB_HOST [env...]: enrolls a node into a fresh volume, writes update.json, starts 0.0.1
new_node() {
  local n=$P-$1 gh=$2; shift 2
  docker volume create "$n-data" >/dev/null; docker volume create "$n-bin" >/dev/null
  docker run --rm -v "$n-bin:/b" -v "$T/out:/src:ro" "$BASE_IMG" cp /src/ulakd-0.0.1 /b/ulakd || die "bin volume"
  otp node
  docker run --rm --network "$NET" -v "$n-data:/data" -v "$n-bin:/b" -e ULAKD_DIR=/data \
    -e ULAKD_OTP="$OTP" -e ULAKD_FINGERPRINT="$FP" "$BASE_IMG" /b/ulakd join "https://$HUB:8443" >/dev/null || die "node join"
  docker run --rm -i -v "$n-data:/data" "$BASE_IMG" sh -c "cat > /data/update.json" <<J
{"repo":"o/r","api_base":"http://$gh:8000","auto_update":false,"interval_hours":1}
J
  docker run -d --name "$n" --restart=always --network "$NET" \
    -v "$n-data:/data" -v "$n-bin:/b" -e ULAKD_DIR=/data "$@" "$BASE_IMG" /b/ulakd run >/dev/null || die "node run"
}
probe() { "$D/examples/ui_probe" "$URL" "$T/ui" "$@"; }
restarts() { docker inspect -f '{{.RestartCount}}' "$P-$1"; }
rm_node() { docker rm -f "$P-$1" >/dev/null 2>&1; }

echo "== (a) UI-triggered update"
new_node a "$GH"
OUT=$(probe 60 --update); echo "$OUT" | tail -8
if grep -q '^VERSION .* 99.0.0' <<<"$OUT" && [ "$(restarts a)" -ge 1 ]; then pass "(a) UI-triggered update"
else fail "(a) UI-triggered update" "restarts=$(restarts a)"; docker logs "$P-a" 2>&1 | tail -5; fi
rm_node a

echo "== (b) auto update"
new_node b "$GH" -e ULAKD_AUTO_UPDATE=1
OUT=$(probe 90); echo "$OUT" | tail -8
if grep -q '^VERSION .* 99.0.0' <<<"$OUT"; then pass "(b) auto update"
else fail "(b) auto update" "no 99.0.0 seen"; docker logs "$P-b" 2>&1 | tail -5; fi
rm_node b

echo "== (c) tampered tarball"
new_node c "$BAD"
OUT=$(probe 25 --update); echo "$OUT" | tail -8
if grep -q '^STATUS .*failed.*[Ss]ignature' <<<"$OUT" && ! grep -q '^VERSION .* 99' <<<"$OUT" \
   && grep -q '^VERSION .* 0.0.1' <<<"$OUT" && [ "$(restarts c)" = 0 ]; then pass "(c) tampered tarball rejected"
else fail "(c) tampered tarball rejected" "restarts=$(restarts c)"; fi
rm_node c

echo "== (d) container default"
docker volume create "$P-d-data" >/dev/null; docker volume create "$P-d-bin" >/dev/null
docker run --rm -v "$P-d-bin:/b" -v "$T/out:/src:ro" "$BASE_IMG" cp /src/ulakd-0.0.1 /b/ulakd || die "bin volume"
docker run --rm --network none -v "$P-d-data:/data" -v "$P-d-bin:/b" -e ULAKD_DIR=/data "$BASE_IMG" /b/ulakd update --check >/dev/null 2>&1
J=$(docker run --rm -v "$P-d-data:/data" "$BASE_IMG" cat /data/update.json 2>&1)
if grep -Eq '"auto_update": *false' <<<"$J"; then pass "(d) container default auto_update=false"
else fail "(d) container default" "$J"; fi

echo
[ "$FAILS" = 0 ] && echo "ALL PASS" || echo "$FAILS FAILED"
exit $((FAILS > 0))
