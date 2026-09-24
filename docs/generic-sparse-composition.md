# Generic Sparse Composition

This note describes how `layerstack` resolves sparse attribute values today,
how that relates to the general framework proposed in
`OpenUSD-proposals/proposals/sparse-array-edits/README.md`, and what remains
open.

## Current State

`layerstack` supports sparse array edits as authored values:

- `Value::ArrayEdit` / `ArrayEdit` (`layerstack/src/array_edit.rs`)
- typed property metadata in `PropertyType`, used for `minsize`/`resize` fill

Sparse resolution lives in `layerstack/src/value_resolution.rs`, not in
`Stage`. `Stage` only linearizes opinions, fetches the schema fallback and
property type, calls `resolve_sparse_value`, and packages provenance.

The strong-over-weak fold itself is `opinionated`'s family kernel
(`opinionated/src/family.rs`):

- `OpinionFamily` is the family contract: `classify` an authored operation
  into `FamilyMember::{Dense, Sparse, Block, Foreign}`, `apply` one edit over a
  weaker dense value, and provide the weakest `seed`.
- `resolve_family_chain` walks strongest to weakest, accumulates sparse edits
  until a dense member or block ends the fold, then applies them weakest-first
  over the dense value or the seed. It consumes the chain lazily, so opinions
  hidden behind the terminating member are never classified.

`layerstack`'s `ArrayFamily` implements `OpinionFamily<Opinion>`:

- `Value::Array` is dense, `Value::ArrayEdit` is sparse, and `Value::Blocked`
  is a block, whether authored as a default or as a time sample.
- Time sampling happens inside `classify`, so the kernel stays time-agnostic
  and only opinions the fold reaches are sampled.
- The seed is the schema fallback when supplied (materialized over `[]` if it
  is itself an edit), otherwise the empty array.

Resolution covers:

- sparse edit over dense array, and sparse over sparse (associative, pinned by
  `sparse_over_sparse_fold_matches_grouped_composition`)
- schema fallback as the weakest dense seed for default-value queries
- held time-sampled sparse edits, with layer offsets applied per opinion
- value blocks, authored or sampled

### Block semantics

A block discards every weaker authored opinion (AOUSD Core §12.3.6). If
nothing stronger contributed, the result is blocked and the caller falls back
to the schema fallback. Sparse edits stronger than the block compose over the
weakest dense value that survives it: the fallback seed, or the empty array,
since the proposal requires a resolved array to always be dense.

## Relation To The Proposal

The proposal generalizes value resolution to any value type with an
`isDense` predicate and an associative `over` operator. The kernel expresses
exactly that shape, and a second family defined outside `opinionated`
(`opinionated/tests/custom_family.rs`) folds over it, so the seam is proven
beyond arrays. Within `layerstack`, arrays remain the only sparse family:
no second sparse value type (path expressions with `%_`, sparse dictionaries)
exists in the core model yet.

Not every family goes through the `OpinionFamily` kernel, and not every
shared algorithm is a family:

- **Dictionaries** are delegated to `opinionated`, but to its dedicated
  recursive combiner (`combine_dictionary_chain` with a `DictionaryAdapter`
  that `layerstack` implements for `Value`), not to the family kernel.
  The kernel applies accumulated edits weakest-first, which is only valid for
  an associative `over`. Recursive dictionary combining (AOUSD Core §6.6.2.1)
  is not associative when a key holds a dictionary in one opinion and a
  non-dictionary in another, and OpenUSD folds such chains strongest-first
  (AOUSD Core §4.2; `MetadataValueComposer` in `pxr/usd/usd/stage.cpp`).
  `layerstack` still selects the participating opinions: a block discards
  weaker ones, and a schema fallback is the weakest dictionary.
- **List ops** share `opinionated`'s `ListOp` implementation directly.
- **Scalars** stay strongest-wins in `Stage`; routing them through the kernel
  would add indirection without removing code.

A family should move onto the kernel when that removes code or adds a
capability, not for uniformity alone.

## Open Gaps

- **Linear interpolation of sparse series.** The proposal evaluates
  interpolated sparse values by composing bracketing samples
  (`GetBracketingSamples`, shifting the query to `hi.time` after a dense
  `lo`). `layerstack` samples each opinion independently and arrays never
  lerp, so array values are effectively held under linear interpolation.
  This matches the proposal for held interpolation but not for numeric
  arrays under linear interpolation.
- **Schema fallback at a time.** `Stage::resolve_value_at_time` does not seed
  sparse resolution with a schema fallback; edits materialize over `[]`.
- **Value clips.** Not implemented, so clip series do not participate in the
  linearization.
- **Diagnostics.** `resolve_family_chain_report` is available but unused;
  `Stage` reports the strongest opinion as provenance rather than the full
  contributing chain the proposal's `UsdResolveInfoSourceComposed` describes.

## What Should Not Change

- sparse opinions are authored values, not out-of-band resolver state
- resolved public values stay dense
- property typing is preserved close to authored fields
- schema fallback participates as the weakest dense seed for sparse families
- the kernel stays time-agnostic; sampling belongs to the domain family

## Practical Reading

- Runnable example:
  `cargo run -p layerstack_examples --example sparse_array_edits`
- Array family and query modes: `layerstack/src/value_resolution.rs`
- Family kernel: `opinionated/src/family.rs`
- Sparse edit kernel: `layerstack/src/array_edit.rs`
