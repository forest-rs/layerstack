#!/usr/bin/env bash
# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Builds the default module into wasm/: `layerstack_wasm` for
# wasm32-unknown-unknown with the size-tuned `wasm` profile, without `std`
# and with the `standalone` feature (its own allocator and a panic handler
# that traps), the glue with
# the wasm-bindgen CLI (which must be the version Cargo.lock holds), then
# `wasm-opt -Oz --converge --low-memory-unused`, with the functions laid out
# so that similar ones sit together. Set WASM_BINDGEN and WASM_OPT to use
# binaries off PATH.
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
# The `wasm` profile keeps function names for `wasm-opt` to order by, and
# with them the `target_features` section, which would switch wasm-bindgen
# to its reference-types glue; take that section out, as `strip = true` did.
module="$target/wasm32-unknown-unknown/wasm/layerstack_wasm.wasm"
node "$package/scripts/strip-sections.mjs" "$module" "${module%.wasm}.bindgen-input.wasm" target_features
rm -rf "$package/wasm"
"$wasm_bindgen" --target web --out-dir "$package/wasm" --out-name layerstack \
  "${module%.wasm}.bindgen-input.wasm"
# `check-size.mjs` reports this copy as `raw`: the module before `wasm-opt`,
# without the names.
node "$package/scripts/strip-sections.mjs" "$package/wasm/layerstack_bg.wasm" \
  "$package/wasm/layerstack_bg.unoptimized.wasm" name
# The features rustc enables by default for wasm32-unknown-unknown.
# `--converge` repeats the passes while the module still shrinks: one -Oz
# leaves inlining and merging opportunities that a second round takes.
# `--low-memory-unused` lets wasm-opt fold a constant under 1 KiB added to
# an address into the access's offset. The two differ only when the
# addition wraps past 4 GiB into the first 1 KiB, which in-bounds Rust
# pointer arithmetic never does (that 1 KiB is the far end of the stack,
# which starts memory and grows down to it).
# `--reorder-functions-by-name` then lays the functions out by their Rust
# names, which puts the functions of one module, and the instances of one
# generic type's methods, next to each other, where gzip and brotli find
# their repeats. `order-functions.mjs` refines that order by the functions'
# bodies, renaming each function to its place, and a second `wasm-opt` run
# moves them there without optimizing again; the optimize and shrink levels
# and the StackIR flags keep its output as compact as the first run's. The
# order changes no behaviour; the output leaves the names out.
features="--enable-bulk-memory --enable-mutable-globals --enable-nontrapping-float-to-int \
  --enable-sign-ext --enable-reference-types --enable-multivalue"
ordered="${module%.wasm}.ordered.wasm"
# shellcheck disable=SC2086 # `features` is a list of flags.
"$wasm_opt" -Oz --converge --low-memory-unused --reorder-functions-by-name -g $features \
  "$package/wasm/layerstack_bg.wasm" -o "$ordered"
node "$package/scripts/order-functions.mjs" "$ordered" "$ordered"
# shellcheck disable=SC2086
"$wasm_opt" --optimize-level=2 --shrink-level=2 --generate-stack-ir --optimize-stack-ir \
  --reorder-functions-by-name $features "$ordered" -o "$package/wasm/layerstack_bg.wasm"
