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
OpenUSD's rules (`USD_LINEAR_INTERPOLATION_TYPES`, `_LerpVisitor` in
`pxr/usd/usd/interpolators.cpp`):

- floating-point scalars, vectors, matrices and time codes interpolate;
- half-precision ones round each result to half precision, as `GfHalf`
  arithmetic does;
- quaternions interpolate by `GfSlerp` in their own precision (`libm`
  supplies `acos` and `sin` without `std`, identically on every target);
- integers hold, arrays of different sizes hold, and a blocked upper sample
  holds the lower one.

Each type rounds as `GfLerp` or `GfSlerp` does in its own arithmetic, bit for
bit: scalars lerp in double precision and narrow once, while float and half
vectors narrow each scaled component before adding (the
`*_interpolate_bit_exact` vectors). `quatd` matches to a few units in the last
place: its slerp keeps the last-place error of `acos` and `sin`, which
differs between C libraries.

Before the first composed time sample, a default or fallback is the lower
sample at `-inf` and holds. Scalar attributes, which do not compose, hold or
interpolate the strongest series' samples with the same element rules and the
same `1e-6` tolerance.

### Block semantics

A block discards every weaker authored opinion (AOUSD Core §12.3.6), and a
blocked time sample does so wherever it is the held sample (§16.2.16.3: "the
same semantics as when blocking the default attribute value"). If nothing
stronger contributed, the result is blocked and a schema-aware query resolves
the schema fallback. Sparse edits stronger than the block compose over the
weakest dense value that survives it: the fallback seed, or the empty array,
since the proposal requires a resolved array to always be dense.

### Schema fallbacks

`Stage::resolve_value_with_schema` (default time) and
`Stage::resolve_value_at_time_with_schema` (numeric times) share one
fallback contract (AOUSD Core §12.3.5, §13.3.2.4):

- with nothing authored at the query time, a spec without a value included,
  the fallback resolves;
- an array fallback is the weakest dense seed of sparse edits, whether they
  compose over no weaker opinion or over a block;
- a block in effect, default or sampled, resolves the fallback (Core
  §12.3.6);
- otherwise authored values resolve as without a schema, and opinions hidden
  behind a dense value or block are never evaluated.

`Stage::resolve_value_at_time` and `Stage::resolve_property_path_at_time` take
no schema: they compose edits over the empty array and resolve a block to no
value.

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

`layerstack_conformance/tests/temporal_sparse.rs` replays vectors recorded
from OpenUSD 26.08 (`scripts/temporal_sparse_oracle.py`, including ports of
`testUsdAttributeArrayEdits.cpp`): mixed dense and sparse samples at differing
times, held and linear interpolation, sublayer and reference offsets, sampled
and default blocks, defaults under and over samples, element types of arrays
and scalars, and schema fallbacks (`Cube`'s `extent` and `size`, recorded from
OpenUSD's schema registry) under sparse samples, blocks and layer offsets. It
also checks that resolving the composed stage equals
resolving OpenUSD's flattened layer.

Every recorded value must match OpenUSD, except for the named divergences
below.

### Divergences From OpenUSD

A divergence is a result Layerstack resolves differently from OpenUSD on
purpose, following the Core specification or the sparse-array-edits proposal
where OpenUSD disagrees with them, or with its own flattened stage. Each has a
name and the OpenUSD release it was confirmed against. The oracle script
records them in `DIVERGENCES`, and each affected query names one and pins
Layerstack's `expected` value. The script refuses to write vectors when:

- OpenUSD no longer shows a divergence, so the override must go;
- the running OpenUSD release is not the confirmed one, so the source lines
  must be re-checked and the version bumped;
- no case shows a named divergence any more.

`divergences_are_exactly_the_named_ones` requires the vectors to name exactly
the divergences and versions below, each shown by a query where OpenUSD's
value differs from the expected one. Any other difference from OpenUSD fails
`composed_resolution_matches_openusd`.

| Name | Confirmed with | OpenUSD | Layerstack | Authority |
| --- | --- | --- | --- | --- |
| `nan-before-first-sample` | 26.08 | NaN for interpolating types before the first composed sample | holds the first composed sample, as OpenUSD's flattened stage does | Core §12.5.1 |
| `override-early-stop` | 26.08 | drops weaker opinions from the interpolated upper sample after the query moves to it | keeps composing weaker series at the upper sample's time | proposal's `Evaluate`; Core §12.3.2 |
| `transparent-sampled-block` | 26.08 | a held sampled block in a weaker series lets opinions weaker than it show through | the block ends the fold | Core §12.3.6 |
| `sampled-block-drops-fallback` | 26.08 | a held sampled block resolves no value despite a schema fallback, and edits over it compose over `[]` | the fallback resolves, and edits compose over it, as for a default block | Core §12.3.6, §16.2.16.3 |
| `default-time-block-hides-fallback` | 26.08 | at the default time a default block resolves no value despite a schema fallback | the fallback resolves, as at numeric times | Core §12.3.6, §16.2.16.2 |

- **`nan-before-first-sample`.** A default or fallback under time samples is a
  sample at `-inf` (`pxr/usd/usd/stage.cpp:8029` and `:8054`,
  `_GetValueFromResolveInfoImpl`), and `Usd_Interpolate` interpolates towards
  the first sample with `alpha = inf / inf`
  (`pxr/usd/usd/interpolators.cpp:125`), giving NaN for interpolating element
  types under held and linear interpolation alike. Core §12.5.1 says queries
  before the first sample return its value.
- **`override-early-stop`.** After a dense lower sample moves the query to the
  upper sample, OpenUSD stops at a weaker series whose own upper sample is
  dense although its sample held at the query time is sparse
  (`pxr/usd/usd/stage.cpp:9032`, `ProcessLayerAtTime`), so weaker opinions
  drop out of the interpolated upper sample, which then differs from the value
  at that sample's own time. Layerstack keeps composing, as the proposal's
  `Evaluate` ("Evaluating a Strength-Ordering of Samples at a Specific Time")
  does.
- **`transparent-sampled-block`.** A weaker series whose lower bracketing
  sample is a block and whose upper sample is sparse contributes no samples in
  OpenUSD (`pxr/usd/usd/interpolators.cpp:163` and `:180`,
  `_GetInterpolatingSamplesImpl`), while the walk continues past it, so
  opinions weaker than the block show through. A block discards weaker
  opinions, sampled ones included (Core §12.3.6), so Layerstack lets it end
  the fold.
- **`sampled-block-drops-fallback`.** OpenUSD sends a default block on to the
  schema fallback (`pxr/usd/usd/stage.cpp:9094`, `ProcessLayerAtTime`), but a
  time-sample source never reaches it: edits stronger than a held sampled
  block compose over `VtBackground`, the empty array (`:8066`), and a
  strongest held sampled block resolves no value (`:8078`,
  `Usd_ClearValueIfBlocked`). Core §12.3.6 resolves a blocked attribute to its
  fallback "at any time", and §16.2.16.3 gives blocked samples the semantics of
  a blocked default, so Layerstack treats both blocks alike.
- **`default-time-block-hides-fallback`.** At the default time OpenUSD reads
  the strongest `default` field and clears a block without consulting the
  fallback (`pxr/usd/usd/stage.cpp:7262` and `:7272`,
  `Usd_AttrGetValueHelper::GetValue`), although its resolve info names the
  fallback as the source (`:9173`) and numeric times resolve the fallback.
  Core §12.3.6 and §16.2.16.2 ("`int x = None` ... only resolve to fallback")
  resolve the fallback.

## Open Gaps

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
- OpenUSD differential vectors: `layerstack_conformance/tests/temporal_sparse.rs`
- Family kernel: `opinionated/src/family.rs`
- Sparse edit kernel: `layerstack/src/array_edit.rs`
