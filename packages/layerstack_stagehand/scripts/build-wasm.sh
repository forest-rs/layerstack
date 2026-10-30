#!/usr/bin/env bash
# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Builds the stagehand module into wasm/: `layerstack_stagehand_wasm` for
# wasm32-unknown-unknown with the size-tuned `wasm` profile, without `std`
# and with the `standalone` feature (its own allocator and a panic handler
# that traps), the glue with the wasm-bindgen CLI (which must be the version
# Cargo.lock holds), then `wasm-opt -Oz`. Set WASM_BINDGEN and WASM_OPT to
# use binaries off PATH. As `packages/layerstack/scripts/build-wasm.sh`,
# without its function ordering: this module is small.
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
cargo build --manifest-path "$workspace/Cargo.toml" --locked -p layerstack_stagehand_wasm \
  --no-default-features --features standalone --profile wasm --target wasm32-unknown-unknown
# The `wasm` profile keeps function names, and with them the
# `target_features` section, which would switch wasm-bindgen to its
# reference-types glue; take that section out.
module="$target/wasm32-unknown-unknown/wasm/layerstack_stagehand_wasm.wasm"
node "$workspace/packages/layerstack/scripts/strip-sections.mjs" "$module" "${module%.wasm}.bindgen-input.wasm" target_features
rm -rf "$package/wasm"
"$wasm_bindgen" --target web --out-dir "$package/wasm" --out-name layerstack_stagehand \
  "${module%.wasm}.bindgen-input.wasm"
# `check-size.mjs` reports this copy as `raw`: the module before `wasm-opt`.
node "$workspace/packages/layerstack/scripts/strip-sections.mjs" "$package/wasm/layerstack_stagehand_bg.wasm" \
  "$package/wasm/layerstack_stagehand_bg.unoptimized.wasm" name
# The features rustc enables by default for wasm32-unknown-unknown.
features="--enable-bulk-memory --enable-mutable-globals --enable-nontrapping-float-to-int \
  --enable-sign-ext --enable-reference-types --enable-multivalue"
# shellcheck disable=SC2086 # `features` is a list of flags.
"$wasm_opt" -Oz --converge $features "$package/wasm/layerstack_stagehand_bg.wasm" \
  -o "$package/wasm/layerstack_stagehand_bg.wasm"
