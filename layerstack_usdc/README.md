# layerstack_usdc

Read USDC binary scene descriptions into Layerstack layers and write
deterministic USDC files from authored documents, layers, or low-level specs.

[API documentation](https://docs.rs/layerstack_usdc) ·
[Source](https://github.com/forest-rs/layerstack/tree/main/layerstack_usdc)

## Installation

```toml
[dependencies]
layerstack_usdc = "0.1"
layerstack_usda = "0.1" # Shared authored-document API used below.
```

Requires Rust **1.88** or later. Uses `no_std` with `alloc`; an allocator is
required. Default features are empty. The declared `std` feature currently
adds no APIs. Callers read and write files themselves; APIs consume byte
slices and return owned data.

## Write a document

```rust
use layerstack_usda::writer::{Attribute, Document, Prim, Value};

let mut root = Prim::def("Xform", "Root");
root.push_property(Attribute::new(
    "greeting", "string", Value::String("hello".into()),
).custom());

let mut document = Document::new();
document.default_prim = Some("Root".into());
document.prims.push(root);
let bytes = layerstack_usdc::writer::write_document(&document).unwrap();
assert_eq!(&bytes[..8], b"PXR-USDC");
```

`writer::save_layer` saves an existing `layerstack::Layer` using its interners.
It shares USDA's [layer-save lowering and limitations](https://docs.rs/layerstack_usda/latest/layerstack_usda/save/index.html):
variants and splines are supported; layer relocates, sparse array edits,
path-expression values, and some metadata/value forms are rejected. The binary
document writer additionally rejects unregistered list-op metadata and metadata
of the wrong type; it preserves other unregistered metadata as text or
dictionaries, following the USDA parser's representation. Saving preserves
authored opinions; it does not flatten composition or package referenced assets.

For applications that own crate-level specs and fields, `writer::write_crate`
provides the lower-level route. The caller supplies hierarchy and field
semantics explicitly; this is not a replacement for document validation.

## Read a file

[`read_usdc`](https://docs.rs/layerstack_usdc/latest/layerstack_usdc/fn.read_usdc.html)
takes the complete file as `&[u8]`, a layer ID, token/path interners, and a
Layerstack `AssetResolver`. Its result includes the authored layer, externally
resolved layers to insert into the caller's store, and diagnostics for authored
content that could not be represented. Inspect those diagnostics as well as the
outer `Result` before assuming a complete import.

The reader accepts crate versions **0.7 through 0.15**, with feature-specific
limits described in the [version module](https://docs.rs/layerstack_usdc/latest/layerstack_usdc/version/index.html).
The writer starts at **0.8.0**, upgrades to **0.9.0** for timecode values,
and to **0.12.0** for splines;
reader version support does not imply every feature of that version is writable.

Decoding uses an input-derived `DecodeBudget`. Use `read_usdc_within` to supply
an explicit budget for materialized data. Files larger than 4 GiB are unsupported
on 32-bit targets. Asset resolution and its resource policy remain the caller's
responsibility.

For ZIP packaging of the resulting bytes and assets, use `layerstack_usdz`.

## License

See [CHANGELOG.md](CHANGELOG.md) for release status.

Licensed under either [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.
