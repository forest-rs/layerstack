# Changelog

## Unreleased

Initial release.

- The views' building blocks are public, so generated views of schema
  libraries outside this crate share them: `PrimView::read_targets`,
  `PrimEdit::new`, `PrimEdit::from_path_unchecked`, `PrimEdit::write_value`,
  `PrimEdit::write_targets`, the `InstanceView` and `InstanceEdit`
  constructors and accessors, each view's `from_prim_unchecked` (or
  `from_instance_unchecked`) and each edit handle's `from_prim` (or
  `from_instance`). `SchemaEdit::define_prim` defines a prim of a schema
  named at run time, and `SchemaEdit::apply_api` returns the prim's
  `PrimEdit`. The `table` module's `SchemaTables` registers a library's
  static tables.
- Producer bindings capture store affinity at construction. Migration:
  `Procedural::new(recipe, evaluator)` becomes
  `Procedural::new(&store, recipe, evaluator)`. Different interners are rejected
  before resolving store-local paths; invalidation does not rebind a producer.
- `GeneratedMesh::into_validated` retains an immutable snapshot and derived
  extent for repeated preparation without geometry rescans. `MeshPublication`
  adds a `work` field; struct literals must supply its planning counters.
- Producer input queries skip resolution on unchanged records and retain
  relationship-forwarding dependencies. `snapshot` shares a detached output with
  input evidence; guarded application synchronizes and rejects stale consumed
  inputs before authoring. Explicit invalidation also retires delayed evidence.
  `ProceduralWork` adds `query_cache_hits`; struct literals must supply this counter.
- `producer_durability` demonstrates two producers, output repair, source-layer
  replacement, delayed-work rejection and budgeted history recovery. Scheduling,
  generator/resource concurrency and output lifecycle remain application-owned.

- Numeric schema array getters and `Primvar::indices` retain `Arc<Vec<T>>`
  storage. Migration: borrow with `as_slice()` / `iter()` or explicitly copy
  with `as_ref().clone()` when a mutable `Vec<T>` is required. Half and matrix
  representation conversions retain vector return types. Numeric shader-node
  array defaults also return shared owners; slice setters remain compatible.
- Numeric edit handles provide `_owned` and `_shared` setters, including time
  samples, and primvar index setters support the same transfers. Conversion
  helpers document borrowed, shared and explicitly materialized reads.
- `GeneratedMesh::prepare` validates and publishes complete polygon meshes
  through an explicit edit target. Producer manifests govern obsolete-property
  removal; unrelated opinions and unchanged buffers are preserved.
  Validation uses an explicit type at the mapped authored site, permitting
  updates to an unselected Mesh variant while a non-mesh sibling is selected.
  Untyped existing sites still require a composed Mesh.
- The runnable `generated_assets` example and OpenUSD conformance tests cover
  shared authored assets, native and point instances, material overrides,
  indexed seams, masking, source changes, recreation and offset time samples.
- The `usd-proc` feature adds `procedural::Procedural<E>` and application-supplied
  evaluators with tracked composed inputs, dynamic dependency replacement,
  retained generic outputs and work counters. Adoption is additive: existing
  publishers can evaluate a recipe before their existing `GeneratedMesh::prepare`
  call. Evaluation never implicitly authors results or loads Hydra plugins.
- `generated_assets` now evaluates two authored `UsdProc` recipes before
  publication, carries texture references into a material network, and proves
  recipe edits, upstream changes, failed evaluation and sampled/offset inputs
  through USDA/USDC and an independent OpenUSD oracle.
