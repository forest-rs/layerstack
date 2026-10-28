#!/usr/bin/env bash
# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Builds the default module into wasm/: `layerstack_wasm` for
# wasm32-unknown-unknown with the size-tuned `wasm` profile, the glue with
# the wasm-bindgen CLI (which must be the version Cargo.lock holds), then
# `wasm-opt -Oz --converge`. Set WASM_BINDGEN and WASM_OPT to use binaries
# off PATH.
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
  --profile wasm --target wasm32-unknown-unknown
rm -rf "$package/wasm"
"$wasm_bindgen" --target web --out-dir "$package/wasm" --out-name layerstack \
  "$target/wasm32-unknown-unknown/wasm/layerstack_wasm.wasm"
cp "$package/wasm/layerstack_bg.wasm" "$package/wasm/layerstack_bg.unoptimized.wasm"
# The features rustc enables by default for wasm32-unknown-unknown.
# `--converge` repeats the passes while the module still shrinks: one -Oz
# leaves inlining and merging opportunities that a second round takes.
"$wasm_opt" -Oz --converge --enable-bulk-memory --enable-mutable-globals --enable-nontrapping-float-to-int \
  --enable-sign-ext --enable-reference-types --enable-multivalue \
  "$package/wasm/layerstack_bg.unoptimized.wasm" -o "$package/wasm/layerstack_bg.wasm"
