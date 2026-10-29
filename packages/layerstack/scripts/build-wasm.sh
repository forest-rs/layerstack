#!/usr/bin/env bash
# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Builds the default module into wasm/: `layerstack_wasm` for
# wasm32-unknown-unknown with the size-tuned `wasm` profile, without `std`
# and with the `standalone` feature (its own allocator and a panic handler
# that traps), the glue with
# the wasm-bindgen CLI (which must be the version Cargo.lock holds), then
# `wasm-opt -Oz --converge --low-memory-unused`. Set WASM_BINDGEN and
# WASM_OPT to use binaries off PATH.
set -euo pipefail
package=$(cd "$(dirname "$0")/.." && pwd)
workspace=$(cd "$package/../.." && pwd)
wasm_bindgen=${WASM_BINDGEN:-wasm-bindgen}
wasm_opt=${WASM_OPT:-wasm-opt}
locked=$(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/version = |"/, ""); print; exit }' "$workspace/Cargo.lock")
cli=$("$wasm_bindgen" --version | awk '{ print $2 }')
if [ "$cli" != "$locked" ]; then
  echo "wasm-bindgen CLI $cli does not match the locked crate $locked" >&2
  exit 1
fi
target=${CARGO_TARGET_DIR:-$workspace/target}
LAYERSTACK_REVISION=${LAYERSTACK_REVISION:-$(git -C "$workspace" rev-parse --short=12 HEAD 2>/dev/null || echo unknown)} \
  cargo build --manifest-path "$workspace/Cargo.toml" --locked -p layerstack_wasm \
  --no-default-features --features standalone --profile wasm --target wasm32-unknown-unknown
rm -rf "$package/wasm"
"$wasm_bindgen" --target web --out-dir "$package/wasm" --out-name layerstack \
  "$target/wasm32-unknown-unknown/wasm/layerstack_wasm.wasm"
cp "$package/wasm/layerstack_bg.wasm" "$package/wasm/layerstack_bg.unoptimized.wasm"
# The features rustc enables by default for wasm32-unknown-unknown.
# `--converge` repeats the passes while the module still shrinks: one -Oz
# leaves inlining and merging opportunities that a second round takes.
# `--low-memory-unused` lets wasm-opt fold a constant under 1 KiB added to
# an address into the access's offset. The two differ only when the
# addition wraps past 4 GiB into the first 1 KiB, which in-bounds Rust
# pointer arithmetic never does (that 1 KiB is the far end of the stack,
# which starts memory and grows down to it).
"$wasm_opt" -Oz --converge --low-memory-unused --enable-bulk-memory --enable-mutable-globals --enable-nontrapping-float-to-int \
  --enable-sign-ext --enable-reference-types --enable-multivalue \
  "$package/wasm/layerstack_bg.unoptimized.wasm" -o "$package/wasm/layerstack_bg.wasm"
