# layerstack_usda

Parse USDA scene descriptions into a lossless concrete syntax tree or a typed
AST, import authored layers into Layerstack, and write deterministic USDA text.

[API documentation](https://docs.rs/layerstack_usda) ·
[Source](https://github.com/forest-rs/layerstack/tree/main/layerstack_usda)

## Installation

```toml
[dependencies]
layerstack_usda = "0.1"
```

Requires Rust **1.89** or later. Uses `no_std` with `alloc`; an allocator is
required. Default features are empty. The declared `std` feature currently
adds no APIs; file I/O and asset loading belong to the caller.

## Read and write

```rust
use layerstack::{InMemoryStore, LayerId, NoAssets};
use layerstack_usda::{read_usda, writer::{Attribute, Document, Prim, Value}};

let mut root = Prim::def("Xform", "Root");
root.push_property(Attribute::new(
    "greeting", "string", Value::String("hello".into()),
).custom());

let mut document = Document::new();
document.default_prim = Some("Root".into());
document.prims.push(root);
let text = document.to_usda().unwrap();

let store = InMemoryStore::default();
let read = read_usda(&text, LayerId(1), store.tokens(), store.paths(), &mut NoAssets);
assert!(read.diagnostics().next().is_none());
```

`read_usda` reads a layer with Layerstack interners and an `AssetResolver`,
which supplies external layers; the reader does not open files. It recovers
from malformed input: inspect `diagnostics()` (parsing, lowering and emission,
in that order) or `has_errors()` before treating the result as valid, and
`is_rejected()` for a layer the reader refuses as OpenUSD's does. It imports
numeric scalar, vector and quaternion property arrays directly into native
buffers in one numeric pass, including samples and variant branches, without
retaining per-element tokens, syntax nodes or generic AST tuples; `stats`
reports retained syntax and recognized numeric arrays. The importer still
retains structural syntax for declarations and metadata; the caller supplies
the complete source string.

The syntax layers (lexer, lossless tree, AST, emitter) are internal. The
`unstable-internals` feature makes them public modules for this repository's
tests, benchmarks and fuzzing; they follow no semver promise.

Metadata statements read as their field's registered type, as OpenUSD's text
parser reads them: the fields Sdf and usd-core's own plugins register are
built in, and for plugin-defined metadata build a `layerstack::SchemaRegistry`
with the same token interner and use `read_usda_with_registry`. A field
registered for the kind of spec
a statement is on converts its literal (`irIsInvertible = 0` is `false` for a
`bool` field); a literal the type cannot hold rejects the layer with a
diagnostic naming the field, its type and why (`Diagnostic::metadata`). A name
registered for no field of that spec keeps its literal's own type, as OpenUSD
keeps such a field's text. Registered defaults remain in the registry;
importing does not author them. `metadata::metadata_value` reads one USDA
literal as a given metadata type.

For storage-backed stages, pass that store and registry to
`layerstack_io::StageDocument::open` through `StageOptions::schemas`.
Initial imports, referenced layers, explicit asset loads, reloads and package
preparation all use the registry. Asset resolution remains caller-owned.

## Saving layers

`writer::Document` is an explicit authoring model. To save an existing
`layerstack::Layer`, use `save::save_usda` with that layer's token and path
interners, or `save::save_usda_to` to stream the same text into any
`core::fmt::Write` a piece at a time, without holding all of it. This preserves
supported authored opinions and asset paths; it does not flatten a composed
stage, copy referenced assets, or retain source formatting.

Saving supports sublayers, references, payloads, inherits, specializes,
variant sets and nested branches, attributes, relationships, time samples,
splines, and typed scalar/array values. The parser and layer model cover more
than the save API: saving currently rejects untyped sparse edits outside attributes and
some metadata/value forms. Native sparse programs retain their literal types
and ordered instructions in both formats. Layer relocates and path expressions are
preserved, including removed destinations and expression arrays.
It also requires `defaultPrim`, when present, to name a locally authored prim.
See the [save support and rejection list](https://docs.rs/layerstack_usda/latest/layerstack_usda/save/index.html)
for the precise boundary. Unsupported data returns an error before output.

To export a composed stage, use `Stage::flatten` in `layerstack`, inspect its
report and verification results, then serialize its output layer. Flattening
bakes sparse array edits into dense values; source-layer saving preserves
authored opinions and keeps its stricter representation limits.

For binary output use `layerstack_usdc`; for ZIP packaging use `layerstack_usdz`.

## License

See [CHANGELOG.md](CHANGELOG.md) for release status.

Licensed under either [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.
