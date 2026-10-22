#!/usr/bin/env bash
# Build chunky-wasm and stage it for wrangler (dist/ is git-ignored).
#   hostlock shared --label "chunky: worker build" -- worker/build.sh
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
cd "$here/.."
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --target wasm32-unknown-unknown --profile wasm -p chunky-wasm
mkdir -p "$here/dist"
cp target/wasm32-unknown-unknown/wasm/chunky_wasm.wasm "$here/dist/chunky_wasm.wasm"
ls -l "$here/dist/chunky_wasm.wasm"
