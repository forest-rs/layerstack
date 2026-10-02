# Layerstack and opinionated

Rust libraries for authored data composition. The workspace separates a generic
opinion-resolution kernel from USD scene composition and file formats.

| Crate | Responsibility |
| --- | --- |
| [`opinionated`](opinionated/) | Dependency-free `no_std + alloc` resolution of ordered opinions, list edits, recursive dictionaries, and typed sparse array edits |
| [`layerstack`](layerstack/) | USD layer stacks, composed prims, references, payloads, inherits, specializes, variants, relocates, atomic authoring, flattening, value queries, and provenance |
| [`layerstack_schemas`](layerstack_schemas/) | Generated OpenUSD schema definitions and typed views, authoring, transforms, bounds, collections, bindings, shading sources, and retained queries |
| [`layerstack_usda`](layerstack_usda/) | USDA text parsing, authored-layer import, and writing |
| [`layerstack_usdc`](layerstack_usdc/) | USDC binary reading and writing |
| [`layerstack_usdz`](layerstack_usdz/) | USDZ archive reading and packaging |
| [`layerstack_mesh_export`](layerstack_mesh_export/) | Meshes, Preview Surface materials, and static point instancers exported to USD |

## Getting started

For settings, document overrides, or another host's typed data, start with
[opinionated's guide](opinionated/README.md). For USD composition over an
in-memory or custom layer store, start with [Layerstack's guide](layerstack/README.md).
File parsing and serialization belong to the companion crates; the core does
not load files, render scenes, or supply application-specific design semantics.

All library crates require Rust **1.89** or later and use `alloc`. See each
crate's documentation for features and supported operations.

## Status and conformance

Layerstack implements a growing subset of USD composition, guided by the
[AOUSD Core specification](https://openusd.org/release/spec_usdcore.html) and
OpenUSD behavior. It is not a fully conformant replacement for OpenUSD.
The upstream ordered composition fixtures are covered by the strict harness,
including implied classes, specializes, repeated arc occurrences, ancestral
arcs, variants, and relocates. Value clips are not evaluated. Format writers
and schema queries have additional limits documented by their crates.

The [strict composition harness](layerstack_conformance/tests/composition_strict.rs)
compares ordered prim/property stacks and values to upstream `pcp.txt` oracles
and records known mismatches explicitly. Separate differential tests cover
file formats, authoring, time queries, and mesh export. Passing these fixtures
is evidence for the tested subset, not a claim of complete USD support.

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo run -p layerstack_examples --example minimal
cargo run -p layerstack_examples --example light_rig_slots
```

- [`layerstack_examples`](layerstack_examples/) contains runnable examples.
- [`layerstack_conformance`](layerstack_conformance/) contains fixture and interoperability tests.
- [`layerstack_schemagen`](layerstack_schemagen/) regenerates schema tables and views.
- [`wind_tunnel`](wind_tunnel/) contains benchmarks.
- [`docs`](docs/) contains design and implementation notes.
- `core-spec-supplemental-release_dec2025/` contains upstream test materials;
  `specs/` contains reference specifications. These are not part of the library packages.

## License

Hand-written Rust code is licensed under either [Apache-2.0](LICENSE-APACHE) or
[MIT](LICENSE-MIT), at your option. Generated OpenUSD schema tables and views
also carry the [Tomorrow Open Source Technology License 1.0](layerstack_schemas/LICENSE-TOST-1.0)
and [notice](layerstack_schemas/NOTICE). Crate packages include their license
files; vendored upstream materials retain their own notices.
