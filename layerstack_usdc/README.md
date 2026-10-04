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

Requires Rust **1.89** or later. Uses `no_std` with `alloc`; an allocator is
required. Default features are empty. The `std` feature enables thread-safe
retained numeric arrays. Callers read and write files themselves; eager APIs
consume byte slices, and retained import takes immutable `Arc<[u8]>` input.

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
variants, splines, layer relocates and path expressions are supported; native
sparse edits are supported too. Some metadata/value forms are rejected. The binary
document writer additionally rejects unregistered list-op metadata and metadata
of the wrong type; it preserves other unregistered metadata as text or
dictionaries, following the USDA parser's representation. Saving preserves
authored opinions; it does not flatten composition or package referenced assets.

For applications that own crate-level specs and fields, `writer::write_crate`
provides the lower-level route. The caller supplies hierarchy and field
semantics explicitly; this is not a replacement for document validation.

Repeated arrays and metadata dictionaries share encoded payloads, including
nested dictionaries. Sharing preserves value types and floating-point bits.

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
to **0.10.0** for path expressions, **0.11.0** for relocates,
**0.12.0** for splines and **0.14.0** for native array edits;
reader version support does not imply every feature of that version is writable.

Decoding uses an input-derived `DecodeBudget`. Use `read_usdc_within` to supply
an explicit budget for materialized data. Files larger than 4 GiB are unsupported
on 32-bit targets. Asset resolution and its resource policy remain the caller's
responsibility.

With the existing `std` feature, `read_usdc_lazy` takes immutable `Arc<[u8]>`
input and assembles the same `Layer`, retaining numeric defaults and time
samples as `TypedArray::Deferred`. Composition and raw value inspection can
keep payloads encoded. `TypedArray::try_materialize` borrows a cached native
buffer or cached error; `Stage::try_resolve_property_path` and its time-query
counterpart materialize the selected array and report failures explicitly.
Ordinary slice accessors work too, returning `None` on decode failure.

The adapter owns a thread-safe, immutable cache, with one decode per distinct
numeric representation and one shared budget across assembly and later reads.
It performs no file I/O or background work. `RetainedValues::stats` exposes
retained input bytes, live sources, cached element bytes, failures, decode
attempts and remaining budget without decoding values. Saving materializes
retained arrays and fails before writing output when one cannot decode.
Caches remain alive through layer/query clones. Demand-all workloads retain
both encoded bytes and decoded buffers, so eager import remains useful when
all values will be needed. File ownership and loading dependencies remain
with the caller; this API does not provide memory mapping or cache eviction.

For ZIP packaging of the resulting bytes and assets, use `layerstack_usdz`.

## License

See [CHANGELOG.md](CHANGELOG.md) for release status.

Licensed under either [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.
