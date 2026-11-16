#!/usr/bin/env bash
# Build the ulak browser bundle (wasm + PWA files) into a folder the hub serves with ULAK_WEB_DIR.
#   scripts/build-web.sh [--out DIR] [--dev]       (default: target/web, release build)
# Needs: rustup target add wasm32-unknown-unknown; cargo install wasm-bindgen-cli --version <Cargo.lock's> --locked
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
out=$root/target/web
profile=release
flags=(--release)
while [[ $# -gt 0 ]]; do
    case $1 in
        --out) out=$2; shift 2 ;;
        --dev) profile=debug; flags=(); shift ;;
        *) echo "usage: build-web.sh [--out DIR] [--dev]" >&2; exit 2 ;;
    esac
done
# The toolchain's own cargo: a wrapper on PATH may lack the wasm32 std.
cargo=${CARGO:-$(rustup which cargo 2>/dev/null || command -v cargo)}
export PATH=$(dirname "$cargo"):$PATH
version=$(python3 -c 'import sys,tomllib; print(next(p["version"] for p in tomllib.load(open(sys.argv[1],"rb"))["package"] if p["name"]=="wasm-bindgen"))' "$root/Cargo.lock")
if ! command -v wasm-bindgen >/dev/null || [[ $(wasm-bindgen --version) != "wasm-bindgen $version" ]]; then
    echo "Install the matching CLI: cargo install wasm-bindgen-cli --version $version --locked" >&2
    exit 2
fi
if [[ $profile == release ]]; then
    # Size over speed; keep the builder's home folder out of panic messages.
    export CARGO_PROFILE_RELEASE_OPT_LEVEL=${CARGO_PROFILE_RELEASE_OPT_LEVEL:-s} \
        CARGO_PROFILE_RELEASE_LTO=${CARGO_PROFILE_RELEASE_LTO:-thin} CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 \
        CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_PROFILE_RELEASE_STRIP=debuginfo
    export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$HOME=/home --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo"
fi
cd "$root"
# --lib: the bin target (`ulak`, native only) would collide with the cdylib's file name.
"$cargo" build -p ulak --lib --target wasm32-unknown-unknown ${flags[@]+"${flags[@]}"}
wasm=${CARGO_TARGET_DIR:-$root/target}/wasm32-unknown-unknown/$profile/ulak.wasm
mkdir -p "$out"
wasm-bindgen --target web --out-dir "$out" --out-name ulak $([[ $profile == release ]] && echo --remove-name-section) "$wasm"
# Rune fetches the full Inter faces from beside the js once the first frame is up.
rune=$("$cargo" metadata --format-version 1 | python3 -c 'import sys,json; print(next(p["manifest_path"] for p in json.load(sys.stdin)["packages"] if p["name"]=="rune"))')
cp "$(dirname "$rune")"/assets/web/Inter-*.ttf "$out/"
# PWA shell shared with the hub crate.
cp "$root"/crates/ulak-remote/web/{manifest.json,sw.js,icon.svg} "$out/"
# The hub CSP forbids inline script and style: the page only references files.
cp "$root"/crates/ulak/web/{index.html,boot.js,style.css} "$out/"
ls -l "$out" | sed 1d
echo "Built $profile bundle in $out. Serve: ULAK_WEB_DIR=$out cargo run -p ulak-remote"
