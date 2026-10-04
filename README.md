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
| [`layerstack_io`](layerstack_io/) | Host-owned storage, dependency loading, retained stage documents, explicit saves, exports and reload policies |
| [`layerstack_mesh_export`](layerstack_mesh_export/) | Meshes, Preview Surface materials, and static point instancers exported to USD |

## Getting started

For settings, document overrides, or another host's typed data, start with
[opinionated's guide](opinionated/README.md). For USD composition over an
in-memory or custom layer store, start with [Layerstack's guide](layerstack/README.md).
For filesystem or custom-transport loading and saving, start with
[layerstack_io](layerstack_io/README.md). Format crates also work directly on
caller-owned bytes. Applications own rendering and execution policy.

All library crates require Rust **1.89** or later and use `alloc`. See each
crate's documentation for features and supported operations.

## Status and conformance

Layerstack implements a growing subset of USD composition, guided by the
[AOUSD Core specification](https://openusd.org/release/spec_usdcore.html) and
OpenUSD behavior. It is not a fully conformant replacement for OpenUSD.
The upstream ordered composition fixtures are covered by the strict harness,
including implied classes, specializes, repeated arc occurrences, ancestral
arcs, variants, and relocates. Runtime value clips use host-loaded source layers;
asset requests and evaluation issues remain explicit. Stage controls include
session layers, layer muting, population masks, payload load rules and atomic
namespace edits. Format writers and schema queries have additional limits
documented by their crates.

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

## Release verification

The eight library crates share version `0.1.0` and Rust 1.89. The conformance
harness, examples, schema generator and benchmarks are repository tools marked
`publish = false`.

Verify the actual library archives, including their registry dependency versions
and license files, before publishing. The archive checker requires Python 3.11
or later:

```sh
cargo package --workspace --exclude layerstack_conformance \
  --exclude layerstack_examples --exclude layerstack_schemagen \
  --exclude wind_tunnel --locked --all-features
python3 .github/check_packages.py "${CARGO_TARGET_DIR:-target}/package"
```

A dependency-compatible publication order is `opinionated`, `layerstack`,
`layerstack_usda`, `layerstack_usdc`, `layerstack_usdz`, `layerstack_schemas`,
`layerstack_io`, then `layerstack_mesh_export`. Packaging verifies the local
release set together; registry publication still requires those dependencies
and appropriate crate ownership. CI runs the same archive verification.

## License

Hand-written Rust code is licensed under either [Apache-2.0](LICENSE-APACHE) or
[MIT](LICENSE-MIT), at your option. Generated OpenUSD schema tables and views
also carry the [Tomorrow Open Source Technology License 1.0](layerstack_schemas/LICENSE-TOST-1.0)
and [notice](layerstack_schemas/NOTICE). Crate packages include their license
files; vendored upstream materials retain their own notices.
