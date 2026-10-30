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
| [`layerstack_wasm`](layerstack_wasm/) | WebAssembly bindings: the native side of the TypeScript package |
| [`@forest-rs/layerstack`](packages/layerstack/) | TypeScript package: open, resolve, inspect, explain, edit and save stages in the browser or Node |
| [`layerstack_stagehand`](layerstack_stagehand/) | Render-ready buffers from a stage: triangulated meshes, face-varying primvars, normals, subset draw ranges and tessellated implicit shapes, with reports |
| [`layerstack_stagehand_wasm`](layerstack_stagehand_wasm/) | WebAssembly module of the stagehand TypeScript package |
| [`@forest-rs/layerstack_stagehand`](packages/layerstack_stagehand/) | TypeScript package: mesh and shape buffers from a stage read through `@forest-rs/layerstack` |
| [`layerstack_schemagen`](layerstack_schemagen/) | Generates the OpenUSD schema tables and views, and compiles a custom `schema.usda` library into `generatedSchema.usda`, `plugInfo.json` and Rust views (`cargo install layerstack_schemagen`) |

## Getting started

For settings, document overrides, or another host's typed data, start with
[opinionated's guide](opinionated/README.md). For USD composition over an
in-memory or custom layer store, start with [Layerstack's guide](layerstack/README.md).
For filesystem or custom-transport loading and saving, start with
[layerstack_io](layerstack_io/README.md). Format crates also work directly on
caller-owned bytes. Applications own rendering and execution policy.

All library crates require Rust **1.89** or later and use `alloc`, except
`layerstack_stagehand` (and its WebAssembly module), which requires **1.92** for
its triangulation dependency and is not published yet. See each crate's
documentation for features and supported operations.

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

The [strict composition harness](layerstack_conformance/tests/conformance/composition_strict.rs)
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
- [`layerstack_schemagen`](layerstack_schemagen/) regenerates schema tables and views, and
  compiles custom schema libraries.
- [`layerstack_custom_schemas`](layerstack_custom_schemas/) is a schema library of its own,
  built as a downstream crate builds one with `layerstack_schemagen compile --rust`.
- [`wind_tunnel`](wind_tunnel/) contains benchmarks.
- [Architecture and authoring](docs/architecture-and-authoring.md) describes ownership,
  mapped edits, undo and publication contracts.
- [Compatibility and validation](docs/compatibility-and-validation.md) explains support
  claims, versioned oracles and failure/recovery evidence.
- [Measurement protocol](wind_tunnel/MEASUREMENT.md) defines reproducible workloads
  and the scope of timing and memory claims.
- `core-spec-supplemental-release_dec2025/` contains upstream test materials;
  `specs/` contains reference specifications. These are not part of the library packages.

## Release verification

The ten published crates and the TypeScript package share version `0.1.0`,
except `opinionated`, which is versioned on its own (`0.2.0`); the crates need
Rust 1.89. The conformance harness, examples, the schema generator's downstream
fixture crate and benchmarks are repository tools marked `publish = false`.

Verify the actual library archives, including their registry dependency versions
and license files, before publishing. The archive checker requires Python 3.11
or later:

```sh
cargo package --workspace --exclude layerstack_conformance \
  --exclude layerstack_examples --exclude wind_tunnel \
  --exclude layerstack_stagehand --exclude layerstack_stagehand_wasm --locked --all-features
python3 .github/check_packages.py "${CARGO_TARGET_DIR:-target}/package"
```

A dependency-compatible publication order is `opinionated`, `layerstack`,
`layerstack_usda`, `layerstack_usdc`, `layerstack_usdz`, `layerstack_schemas`,
`layerstack_io`, `layerstack_mesh_export`, `layerstack_wasm`, then
`layerstack_schemagen`. Packaging verifies the local
release set together; registry publication still requires those dependencies
and appropriate crate ownership. CI runs the same archive verification.

## License

Hand-written Rust code is licensed under either [Apache-2.0](LICENSE-APACHE) or
[MIT](LICENSE-MIT), at your option. Generated OpenUSD schema tables and views
also carry the [Tomorrow Open Source Technology License 1.0](layerstack_schemas/LICENSE-TOST-1.0)
and [notice](layerstack_schemas/NOTICE). Crate packages include their license
files; vendored upstream materials retain their own notices.
