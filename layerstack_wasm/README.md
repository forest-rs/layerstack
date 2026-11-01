# layerstack_wasm

WebAssembly bindings for [Layerstack](https://github.com/forest-rs/layerstack):
the native side of the TypeScript package `@forest-rs/layerstack`
(`packages/layerstack`). Use that package from JavaScript; the exports of this
crate are an ABI for its wrapper, which decodes their records into typed
objects.

The module opens a stage from the bytes of its root layer and never fetches.
The host runs the loop: the stage lists the assets it asks for that the host
has not supplied (unavailable), the host fetches them, supplies their bytes
(or confirms one absent), and the stage reads them, until none is
unavailable. A USDZ package is one supplied source. The module keeps a
supplied source's bytes only until the stage has read a layer from them,
unless the layer keeps views of them (a package does), so a text layer's
source is freed once parsed; an asset wanted again after that is
unavailable until supplied again. The stage then
answers prim, child, attribute, relationship, metadata and value queries, and
explains where a value comes from. Failures carry a stable dotted code
(`io.unavailable`, `path.invalid`).

Edits arrive as an `EditBatch` the wrapper fills from its plain-data `Edit[]`,
and `Stage::apply` lowers it to one atomic `layerstack::Transaction` on the
target, loading what the edits ask for that the host supplied. The target is
a layer of the stage's layer stack, a local variant, or an arc of a composed
prim, such as the reference an opinion comes through: the stage mints each
as a record and finds it again in the prim's current composition when it is
used. It returns an `EditReceipt` holding the guarded inverse; `Stage::undo`
applies it and returns the receipt that redoes. The stage keeps one change cursor, which
the wrapper drains after each mutating call to dispatch change listeners
(JavaScript callbacks cannot satisfy `subscribe_changes`' `Send + Sync`). A
root that cannot be saved, such as a USDZ package, is edited through an
anonymous session layer opened in front of it. Saving returns the bytes of
the dirty layers for the host to persist, and exports return a layer (USDA
or USDC), the flattened stage or a USDZ package, touching nothing.

## Building the module

```sh
cargo build -p layerstack_wasm --no-default-features --features standalone \
  --profile wasm --target wasm32-unknown-unknown
node packages/layerstack/scripts/strip-sections.mjs \
  target/wasm32-unknown-unknown/wasm/layerstack_wasm.wasm \
  target/wasm32-unknown-unknown/wasm/layerstack_wasm.bindgen-input.wasm target_features
wasm-bindgen --target web --out-dir packages/layerstack/wasm --out-name layerstack \
  target/wasm32-unknown-unknown/wasm/layerstack_wasm.bindgen-input.wasm
wasm-opt -Oz --converge --low-memory-unused --reorder-functions-by-name -g \
  packages/layerstack/wasm/layerstack_bg.wasm -o ordered.wasm
node packages/layerstack/scripts/order-functions.mjs ordered.wasm ordered.wasm
wasm-opt --optimize-level=2 --shrink-level=2 --generate-stack-ir --optimize-stack-ir \
  --reorder-functions-by-name ordered.wasm -o packages/layerstack/wasm/layerstack_bg.wasm
```

The `wasm` profile keeps the function names, which the first `wasm-opt` run
lays the functions out by; `order-functions.mjs` then renames them so that
the second run lays functions with similar bodies next to each other, where
gzip and brotli compress them better, and leaves the names out. The
`target_features` section the names come with is taken out by
`strip-sections.mjs`, so that wasm-bindgen writes the same glue as for a
stripped module. (`build-wasm.sh` also passes the target's features to
`wasm-opt`.)

The module is built without `std`: the `standalone` feature supplies its
global allocator (dlmalloc, the one `std` uses on `wasm32-unknown-unknown`)
and a panic handler that traps, as `std`'s does there once it has formatted a
message it has nowhere to write. That also leaves out the core crates' `std`
paths (threads, retained USDC arrays), which the bindings never take.

`packages/layerstack/scripts/build-wasm.sh` runs these steps and checks that
the `wasm-bindgen` CLI is the version `Cargo.lock` holds, which it must be.

## Linking it into another module

A crate can depend on `layerstack_wasm` as an `rlib` and export functions of
its own that take `&layerstack_wasm::Stage`, reaching the document through
`Stage::document` and `Stage::document_mut`. Its generated module then holds
Layerstack's classes and its own exports over one memory; pass that module to
`loadLayerstack` in TypeScript (its type is `NativeModule`, from
`@forest-rs/layerstack/native`), and call its exports with the stage
`stage.withNative` lends. Edits made through the document reach the package's
change listeners and make no undo receipt (see `Stage::document_mut`).

## Versions

This crate and the TypeScript package are released together, at the same
version. The records the module exchanges with the package are versioned by
`ABI`, currently 4; a change to an export's arguments or a record's layout
brings a new ABI, and the package refuses a module of another.

## Minimum supported Rust Version (MSRV)

This version of `layerstack_wasm` has been verified to compile with **Rust 1.89**
and later.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.
