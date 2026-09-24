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

`layerstack`'s `ArrayFamily` implements `OpinionFamily<Opinion>` for
default-time queries:

- `Value::Array` is dense, `Value::ArrayEdit` is sparse, and `Value::Blocked`
  is a block.
- The seed is the schema fallback when supplied (materialized over `[]` if it
  is itself an edit), otherwise the empty array.

Resolution covers:

- sparse edit over dense array, and sparse over sparse (associative, pinned by
  `sparse_over_sparse_fold_matches_grouped_composition`)
- schema fallback as the weakest dense seed
- time-sampled sparse edits, composed at the bracketing samples (below)
- value blocks, authored or sampled

### Time queries

An attribute's opinions at a time compose as series of samples, not as one
interpolated value per opinion: an edit authored at one time composes over
the weaker value held at that time, and interpolation sees composed values.
This follows the proposal's "Evaluating a Strength-Ordering of Samples at a
Specific Time" and OpenUSD's `_GetValueFromResolveInfoImpl`
(`pxr/usd/usd/stage.cpp`):

- Each opinion contributes its samples bracketing the query time, mapped to
  stage time through its layer offset (`LayerOffset::map_time`); a default is
  one sample at `-inf`. Only the lower sample contributes under held
  interpolation, or when the two samples are closer than `1e-6` in layer
  time (OpenUSD's `_GetInterpolatingSamplesImpl`).
- Strongest first, each opinion's samples compose under the composed series
  (`SdfComposeTimeSampleSeries` semantics: stronger samples compose over the
  weaker sample held at their time; a weaker sample shows only where the
  stronger held sample is sparse; samples of the two series closer than
  `1e-6` merge into one), which is then trimmed back to the samples
  bracketing the query time.
- When the composed lower sample is dense or blocked but the upper one is
  sparse, the lower sample hides weaker opinions until the upper sample, so
  weaker opinions are bracketed at the upper sample's time instead.
- The walk stops once neither composed bracketing sample is sparse (for held
  interpolation, once the lower one is not), so hidden opinions are never
  visited.

`plan_brackets` runs that walk without touching values: each composed sample
records its time, whether it is still sparse, and which of every opinion's
bracketing samples it is made of. The time-agnostic kernel then folds the
chain once for the composed lower sample and, for linear interpolation, once
for the upper one, reading exactly those samples (`PickedArrayFamily`). The
`bracket_planning_matches_series_composition` tests cross-check this against
composing the sample values directly, on random chains with whole-frame times
and with times closer than the tolerance.

The composed samples are then held, or interpolated element by element with
OpenUSD's rules: floating-point scalars, vectors, matrices and time codes
interpolate, integers hold, arrays of different sizes hold, and a blocked
upper sample holds the lower one. Before the first composed time sample, a
default or fallback is the lower sample at `-inf` and holds.

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

## Conformance With OpenUSD

Time queries depart from three OpenUSD 26.08 results that are defects; each
disagrees with OpenUSD's own flattened stage or with the value at the
sample's own time:

- **NaN before the first sample.** A default or fallback under time samples
  is a sample at `-inf`, and OpenUSD interpolates towards the first sample
  with `alpha = inf / inf`, giving NaN for interpolating element types.
  `layerstack` holds the composed lower sample.
- **Early stop after moving the query.** After a dense lower sample moves the
  query to the upper sample, OpenUSD stops at a weaker series whose own upper
  sample is dense although its sample held at the query time is sparse, so
  weaker opinions drop out of the interpolated upper sample. `layerstack`
  keeps composing, as the proposal does.
- **Transparent sampled block.** A weaker series whose lower bracketing sample
  is a block and whose upper sample is sparse contributes nothing in OpenUSD,
  and opinions weaker than the block show through. `layerstack` lets the block
  end the fold (AOUSD Core §12.3.6).

## Open Gaps

- **Schema fallback at a time.** The resolver seeds time queries with a schema
  fallback, but `Stage` has no schema-aware time query, so
  `Stage::resolve_value_at_time` materializes edits over `[]`. OpenUSD seeds
  the fallback after a default block but uses the empty array after a sampled
  block; `layerstack` seeds the fallback after either.
- **Interpolating half-precision and quaternion arrays.** OpenUSD interpolates
  them (quaternions by slerp); `layerstack` holds them.
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
- the kernel stays time-agnostic; bracketing and sampling belong to the domain

## Practical Reading

- Runnable example:
  `cargo run -p layerstack_examples --example sparse_array_edits`
- Array family, query modes and time queries:
  `layerstack/src/value_resolution.rs`
- Family kernel: `opinionated/src/family.rs`
- Sparse edit kernel: `layerstack/src/array_edit.rs`
