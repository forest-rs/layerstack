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
use layerstack_usda::{parser, writer::{Attribute, Document, Prim, Value}};

let mut root = Prim::def("Xform", "Root");
root.push_property(Attribute::new(
    "greeting", "string", Value::String("hello".into()),
).custom());

let mut document = Document::new();
document.default_prim = Some("Root".into());
document.prims.push(root);
let text = document.to_usda().unwrap();

let parsed = parser::parse(&text);
assert!(parsed.diagnostics.is_empty());
```

`parser::parse_cst` preserves source syntax, including comments and whitespace.
`parser::parse` lowers that tree to an AST. Both recover from malformed input;
inspect their diagnostics before treating the result as valid. To compose the
authored data, use [`emit::emit`](https://docs.rs/layerstack_usda/latest/layerstack_usda/emit/fn.emit.html)
with Layerstack interners and an `AssetResolver`, and inspect emission diagnostics
as well. The resolver supplies external layers; the parser does not open files.

## Saving layers

`writer::Document` is an explicit authoring model. To save an existing
`layerstack::Layer`, use `save::save_usda` with that layer's token and path
interners. This preserves supported authored opinions and asset paths; it does
not flatten a composed stage, copy referenced assets, or retain source formatting.

Saving supports sublayers, references, payloads, inherits, specializes,
variant sets and nested branches, attributes, relationships, time samples,
splines, and typed scalar/array values. The parser and layer model cover more
than the save API: saving currently rejects layer relocates, sparse array
edits, path-expression values, and some metadata/value forms.
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
