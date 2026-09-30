<div align="center">

# LayerStack

**Layered scene-description composition in Rust, with `no_std` + `alloc`.**

[![Latest published version.](https://img.shields.io/crates/v/layerstack.svg)](https://crates.io/crates/layerstack)
[![Documentation build status.](https://img.shields.io/docsrs/layerstack.svg)](https://docs.rs/layerstack)
[![Apache 2.0 or MIT license.](https://img.shields.io/badge/license-Apache--2.0_OR_MIT-blue.svg)](#license)

</div>

<!-- We use cargo-rdme to update the README with the contents of lib.rs.
To edit the following section, update it in lib.rs, then run:
cargo rdme --workspace-project=layerstack --heading-base-level=0
Full documentation at https://github.com/orium/cargo-rdme -->

<!-- Intra-doc links used in lib.rs may be evaluated here. -->

[`SchemaRegistry`]: https://docs.rs/layerstack/latest/layerstack/struct.SchemaRegistry.html
[`Layer`]: https://docs.rs/layerstack/latest/layerstack/struct.Layer.html
[`PrimSpec`]: https://docs.rs/layerstack/latest/layerstack/struct.PrimSpec.html
[`Stage`]: https://docs.rs/layerstack/latest/layerstack/struct.Stage.html
[`LiveStage`]: https://docs.rs/layerstack/latest/layerstack/struct.LiveStage.html
[`InMemoryStore`]: https://docs.rs/layerstack/latest/layerstack/struct.InMemoryStore.html
[`Path`]: https://docs.rs/layerstack/latest/layerstack/struct.Path.html
[`PropertyPath`]: https://docs.rs/layerstack/latest/layerstack/struct.PropertyPath.html
[`TargetPath`]: https://docs.rs/layerstack/latest/layerstack/struct.TargetPath.html
[`SpecPath`]: https://docs.rs/layerstack/latest/layerstack/struct.SpecPath.html
[`TokenInterner`]: https://docs.rs/layerstack/latest/layerstack/struct.TokenInterner.html
[`PathInterner`]: https://docs.rs/layerstack/latest/layerstack/struct.PathInterner.html
[`PathId`]: https://docs.rs/layerstack/latest/layerstack/struct.PathId.html
[`TokenId`]: https://docs.rs/layerstack/latest/layerstack/struct.TokenId.html
[`Value`]: https://docs.rs/layerstack/latest/layerstack/enum.Value.html
[`FieldValue`]: https://docs.rs/layerstack/latest/layerstack/enum.FieldValue.html
[`LayerStore`]: https://docs.rs/layerstack/latest/layerstack/trait.LayerStore.html
[`AssetResolver`]: https://docs.rs/layerstack/latest/layerstack/trait.AssetResolver.html
[`Stage::composition_errors`]: https://docs.rs/layerstack/latest/layerstack/struct.Stage.html#method.composition_errors
[`StageOptions::with_provenance`]: https://docs.rs/layerstack/latest/layerstack/struct.StageOptions.html#structfield.with_provenance

[`ListOp`]: https://docs.rs/layerstack/latest/layerstack/struct.ListOp.html
[`PathExpression`]: https://docs.rs/layerstack/latest/layerstack/struct.PathExpression.html
[`path_expression`]: https://docs.rs/layerstack/latest/layerstack/path_expression/index.html
[`variable_expression`]: https://docs.rs/layerstack/latest/layerstack/variable_expression/index.html
[`edit`]: https://docs.rs/layerstack/latest/layerstack/edit/index.html
[`StageOptions::schemas`]: https://docs.rs/layerstack/latest/layerstack/struct.StageOptions.html#structfield.schemas
[`Stage::prim_definition`]: https://docs.rs/layerstack/latest/layerstack/struct.Stage.html#method.prim_definition
[`Stage::explain_property_value`]: https://docs.rs/layerstack/latest/layerstack/struct.Stage.html#method.explain_property_value
[`EditTarget`]: https://docs.rs/layerstack/latest/layerstack/struct.EditTarget.html
[`Transaction`]: https://docs.rs/layerstack/latest/layerstack/struct.Transaction.html

<!-- cargo-rdme start -->

`layerstack` composes authored layers into a queryable scene description.
A stronger layer can override a field, edit a list, or select a variant while
preserving the weaker source data. Composition follows the
[OpenUSD Core specification][spec]; the value and schema APIs can also serve
applications outside graphics.

[spec]: https://openusd.org/release/spec_usdcore.html

It provides:

- **Layer stacks** — recursive sublayers with deterministic strength ordering
- **Stage population** — a composed prim tree from all contributing layers
- **Value resolution** — scalars, [`ListOp`] chaining, recursive dictionaries,
  sparse array edits, time samples and splines
- **Composition arcs** — local, inherits, variants, references, payloads,
  specializes, and namespace relocates (LIVERPS)
- **Path expressions** — sets of prim and property paths
  ([`PathExpression`]: globs, `//`, predicates, set operators and
  references to other expressions), composed as values and matched
  with a caller-supplied predicate library ([`path_expression`])
- **Variable expressions** — sublayer, reference and payload asset paths
  and variant selections authored as expressions, evaluated with each
  layer stack's `expressionVariables` ([`variable_expression`])
- **Incremental recomposition** — via [`LiveStage`] and the `invalidation`
  dependency graph
- **Authoring** — via [`edit`]: edit targets through any node of a
  prim's composition, and atomic transactions of spec edits that
  return their inverses and check preconditions
- **Schemas** — prim definitions (type, applied schemas, defined
  properties and their fallbacks) from the [`SchemaRegistry`] a stage is
  composed with ([`StageOptions::schemas`], [`Stage::prim_definition`])
- **Value explanations** — [`Stage::explain_property_value`] and its
  siblings say why a value is what it is: every consulted opinion with its
  layer, spec, arc and layer offset, and whether it contributed a value, a
  sparse array edit, dictionary entries or a list edit, or was shadowed,
  cut off by a block or incompatible (compare OpenUSD's
  `UsdAttribute::GetResolveInfo`)

# Quick start

Add `layerstack = "0.1"` to your dependencies. This example authors a base
layer and a stronger local override, then reads their composed value:

```rust
use layerstack::{
    InMemoryStore, Layer, LayerId, PrimSpec, Stage, StageOptions, SublayerEntry, Value,
};

let mut store = InMemoryStore::default();
let title = store.tokens.intern("title");
let prim = store.path("/Doc");

let mut base = Layer::new(LayerId(1));
base.insert_prim(prim, PrimSpec::def().with_field(title, Value::string("Untitled")));
store.insert_layer(base);

let mut local = Layer::new(LayerId(2));
local.sublayers.push(SublayerEntry::new(LayerId(1)));
local.insert_prim(prim, PrimSpec::over().with_field(title, Value::string("Hello")));
store.insert_layer(local);

let stage = Stage::compose(&mut store, LayerId(2), StageOptions::default());
assert!(stage.composition_errors().is_empty());
let resolved = stage.resolve_field(prim, title).unwrap();
assert_eq!(resolved.value, Value::string("Hello"));
```

Composition returns a stage even when some arcs cannot be resolved. Inspect
[`Stage::composition_errors`] before treating the result as complete. Enable
[`StageOptions::with_provenance`] to include the winning source in resolved values.

# Key types

| Type | Role |
|------|------|
| [`Layer`] / [`PrimSpec`] | Authored layer and prim opinions |
| [`Stage`] | Read-only composed values and prim hierarchy |
| [`LiveStage`] | Incremental edits, change reports, callbacks and change cursors |
| [`InMemoryStore`] / [`LayerStore`] | Built-in storage and a host storage interface |
| [`Value`] / [`FieldValue`] | Authored values and composition containers |
| [`Path`] / [`PropertyPath`] / [`TargetPath`] / [`SpecPath`] | Distinct prim, property, target, and source-spec paths |
| [`TokenInterner`] / [`PathInterner`] | Store-local token and path handles |
| [`SchemaRegistry`] | Schema definitions, prim definitions and fallback values |
| [`EditTarget`] / [`Transaction`] | Mapped, atomic authoring with preconditions and undo |

[`PathId`] and [`TokenId`] belong to their interners; they are not durable
identities to persist or exchange between unrelated stores.

# Scope and compatibility

This crate owns in-memory composition, not file I/O, geometry evaluation, or
material interpretation. Companion crates in the [repository][repo] provide
USDA and USDC readers/writers, USDZ packaging, mesh export, and generated OpenUSD
schema views with transform, bounds and shading queries. Applications supply
asset resolution through [`AssetResolver`] and their own domain behavior.
For composition of already-ordered opinions without USD paths and arcs, see
[`opinionated`](https://docs.rs/opinionated).

OpenUSD compatibility is bounded by the implemented and tested subset. Value
clips are not evaluated. Feature presence is not a guarantee of full OpenUSD
equivalence. The repository's [conformance harness][conformance] records exact
ordered stack and value checks against upstream fixtures and differential tests
for additional behavior.

[`LiveStage`] applies transactions or refreshes explicitly reported host edits.
Recomposition can be scoped to affected prims; edits outside the supported local
paths can require a full rebuild. Change reports expose the work performed.
Callbacks and independent change cursors let consumers observe reported edits;
changes to host storage do not notify the stage automatically.

[repo]: https://github.com/forest-rs/layerstack
[conformance]: https://github.com/forest-rs/layerstack/tree/main/layerstack_conformance

# Features and Rust version

Requires **Rust 1.88** or later. The default feature set is empty, and the crate
uses `no_std` with `alloc` (a global allocator is required). The optional `std`
feature currently adds no composition capabilities.

<!-- cargo-rdme end -->

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE] or <http://www.apache.org/licenses/LICENSE-2.0>), or
- MIT license ([LICENSE-MIT] or <http://opensource.org/licenses/MIT>),

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you,
as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

[LICENSE-APACHE]: LICENSE-APACHE
[LICENSE-MIT]: LICENSE-MIT
