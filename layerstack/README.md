<div align="center">

# LayerStack

**AOUSD-aligned, no_std-friendly composition kernel.**

[![Latest published version.](https://img.shields.io/crates/v/layerstack.svg)](https://crates.io/crates/layerstack)
[![Documentation build status.](https://img.shields.io/docsrs/layerstack.svg)](https://docs.rs/layerstack)
[![Apache 2.0 or MIT license.](https://img.shields.io/badge/license-Apache--2.0_OR_MIT-blue.svg)](#license)

</div>

<!-- We use cargo-rdme to update the README with the contents of lib.rs.
To edit the following section, update it in lib.rs, then run:
cargo rdme --workspace-project=layerstack --heading-base-level=0
Full documentation at https://github.com/orium/cargo-rdme -->

<!-- Intra-doc links used in lib.rs may be evaluated here. -->

<!-- cargo-rdme start -->

`layerstack` is a small, domain-neutral composition kernel aligned with the
[OpenUSD Core specification][spec].

[spec]: https://openusd.org/release/spec_usdcore.html

It provides:

- **Layer stacks** — recursive sublayers with deterministic strength ordering
- **Stage population** — a composed prim tree from all contributing layers
- **Value resolution** — scalar (strongest-wins) and [`ListOp`] chaining
- **Composition arcs** — local, inherits, variants, references, payloads,
  specializes (LIVERPS)
- **Incremental recomposition** — via [`LiveStage`] and the `invalidation`
  dependency graph
- **Schema fallbacks** — via [`SchemaRegistry`]

# Quick start

```rust
use layerstack::{
    InMemoryStore, Layer, LayerId, PrimSpec, Stage, StageOptions, Value,
};

let mut store = InMemoryStore::default();
let title = store.tokens.intern("title");
let prim = store.path("/Doc");

let mut layer = Layer::new(LayerId(1));
layer.insert_prim(prim, PrimSpec::def().with_field(title, Value::string("Hello")));
store.insert_layer(layer);

let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
let resolved = stage.resolve_field(prim, title).unwrap();
assert_eq!(resolved.value, Value::string("Hello"));
```

# Key types

| Type | Role |
|------|------|
| [`Stage`] | Immutable composed stage — query values, traverse prims |
| [`LiveStage`] | Mutable stage with incremental recomposition |
| [`InMemoryStore`] | Simple in-memory [`LayerStore`] implementation |
| [`LayerStore`] | Trait for pluggable layer storage |
| [`Value`] / [`FieldValue`] | Scalar values and field containers |
| [`TokenInterner`] / [`PathInterner`] | Interning for strings and paths |
| [`SchemaRegistry`] | Schema definitions and fallback value lookup |

# `no_std` support

This crate is `no_std` by default (uses `alloc`). Enable the **`std`** feature
for `std::error::Error` integration.

<!-- cargo-rdme end -->

## Minimum Supported Rust Version (MSRV)

This crate has been verified to compile with **Rust 1.88** and later.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE] or <http://www.apache.org/licenses/LICENSE-2.0>), or
- MIT license ([LICENSE-MIT] or <http://opensource.org/licenses/MIT>),

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you,
as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

[LICENSE-APACHE]: https://github.com/forest-rs/layerstack/blob/main/LICENSE-APACHE
[LICENSE-MIT]: https://github.com/forest-rs/layerstack/blob/main/LICENSE-MIT
