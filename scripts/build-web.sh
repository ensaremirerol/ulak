#!/usr/bin/env bash
# Build the ulak browser UI (Preact PWA in web/) into a folder the hub serves with ULAK_WEB_DIR.
#   scripts/build-web.sh [--out DIR]       (default: target/web)
# Needs node 22 + npm.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
out=$root/target/web
while [[ $# -gt 0 ]]; do
    case $1 in
        --out) out=$2; shift 2 ;;
        *) echo "usage: build-web.sh [--out DIR]" >&2; exit 2 ;;
    esac
done
for t in node npm; do
    command -v "$t" >/dev/null || { echo "build-web.sh: '$t' not found; install Node.js 22 (with npm)" >&2; exit 2; }
done
cd "$root/web"
# Reinstall only when node_modules is missing or older than the lockfile.
if [[ ! -f node_modules/.package-lock.json || package-lock.json -nt node_modules/.package-lock.json ]]; then
    npm ci
fi
npm run build
rm -rf "$out"
mkdir -p "$out"
cp -a dist/. "$out/"
ls -l "$out" | sed 1d
echo "Built bundle in $out. Serve: ULAK_WEB_DIR=$out cargo run -p ulak-remote"
