#!/usr/bin/env bash
# The repo's checks (D-023), in order. Locally, run it under a shared hold:
#   hostlock shared --label "chunky: check" -- scripts/check.sh
# CI runs the same script with synthetic fixtures only.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
# Non-interactive shells may not have nvm's Node on PATH; use the newest
# nvm-installed Node 24 if `node` is missing.
if ! command -v node >/dev/null; then
    for d in $(ls -d "$HOME"/.nvm/versions/node/v24.*/bin 2>/dev/null | sort -V -r); do
        export PATH="$d:$PATH"
        break
    done
fi
command -v node >/dev/null || { echo "check.sh: node (24) not found" >&2; exit 1; }

step() { printf '\n== %s\n' "$*"; }

step "cargo fmt --check"
cargo fmt --all --check

step "cargo clippy"
cargo clippy --workspace --all-targets -- -D warnings

step "cargo test"
cargo test --workspace

step "wasm32 build (chunky-wasm and the fork)"
cargo build --target wasm32-unknown-unknown --profile wasm -p chunky-wasm -p brotli

step "Worker stream tests and native-vs-wasm identity"
node --test worker/test/*.test.mjs

step "cargo deny"
cargo deny check licenses bans

step "tools: py_compile and node --check"
# Keep bytecode out of the tree.
export PYTHONPYCACHEPREFIX="${TMPDIR:-/tmp}/chunky-pycache"
find tools -name '*.py' -not -path '*/__pycache__/*' -print0 \
    | xargs -0 -r python3 -m py_compile
js_dirs=(tools)
[[ -d worker ]] && js_dirs+=(worker)
find "${js_dirs[@]}" \( -name '*.js' -o -name '*.mjs' \) -not -path '*/node_modules/*' -print0 \
    | xargs -0 -r -n1 node --check

printf '\nall checks passed\n'
