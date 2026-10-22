#!/usr/bin/env bash
# Build the pinned C google/brotli encoder plus driver.c to wasm32 with
# wasi-sdk 34 (D-023): the Worker CPU baseline, run in V8 under Node's WASI
# shim. Same -O2 -DNDEBUG and no -march as the native build.
#   hostlock shared --label "chunky: c wasm build" -- crates/brotli-c/wasm/build.sh
# Output: target/wasm-c/brotli_c.wasm (+ .flags, the exact command line).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$here/../../.."
src="$root/third_party/google-brotli/c"
sdk="${WASI_SDK:-$HOME/.local/wasi-sdk-34.0-x86_64-linux}"
out="$root/target/wasm-c"
mkdir -p "$out"
flags=(--target=wasm32-wasip1 -O2 -DNDEBUG -mexec-model=reactor -I"$src/include"
       -Wl,--strip-all -Wl,--export=malloc -Wl,--export=free)
"$sdk/bin/clang" "${flags[@]}" -o "$out/brotli_c.wasm" "$here/driver.c" \
    "$src"/common/*.c "$src"/enc/*.c
printf '%s\n' "$("$sdk/bin/clang" --version | head -1)" "${flags[*]}" > "$out/brotli_c.flags"
ls -l "$out/brotli_c.wasm"
