# Generic Sparse Composition

This note explains the gap between the current sparse-array-edit implementation
and the broader architecture proposed in
`OpenUSD-proposals/proposals/sparse-array-edits/README.md`.

## Current State

`layerstack` now supports sparse array edits as authored values:

- `Value::ArrayEdit`
- `ArrayEdit`
- typed property metadata in `PropertyType`

Resolution works correctly for this family:

- sparse array edit over dense array
- sparse array edit over sparse array edit
- schema fallback as weakest dense seed when the field is array-valued
- held/interpolated time-sampled sparse array edits

The implementation seam is explicit in `layerstack/src/stage.rs`:

- `resolve_array_value_chain`
- `resolve_array_value_at_time_chain`
- `compose_array_value_over`
- `materialize_array_value`
- `opinion_can_yield_array_family`

That is the key limitation. `Stage` knows what an array family is and has
dedicated code for it.

## Why This Is Narrower Than The Proposal

The proposal is not really about arrays. Arrays are the first motivating case.
The deeper change is to USD value resolution:

- some authored values are dense and self-sufficient
- some authored values are sparse and require weaker opinions to resolve
- composition should fold those values generically, not as one-off field logic

Our current code still has these array-specific assumptions:

1. Sparse-family detection is hardcoded as
   `Value::Array(_) | Value::ArrayEdit(_)`.
2. Composition dispatch lives in `Stage`, not in a reusable value-resolution
   kernel.
3. Materialization rules are array-specific: unresolved sparse values are
   finalized by composing over `[]` plus typed defaults.
4. Time-sampled sparse resolution is implemented by sampling first and then
   calling array-specific composition helpers.

That means adding another sparse family, such as a generalized path-expression
or future sparse dictionary type, would require reopening `Stage` and adding
another parallel set of branches and helper functions.

## Target Shape

The next refactor should move from “array-specialized sparse resolution” to
“generic sparse-family resolution.”

The right center of gravity is a new internal resolver module in `layerstack`,
not more logic in `Stage`.

Suggested structure:

1. Add `layerstack/src/value_resolution.rs`.
   Responsibility:
   - identify the value family present in an opinion chain
   - fold strong-over-weak opinions for that family
   - materialize any remaining sparse value into a dense result
2. Introduce a small internal family discriminator.
   Example shape:
   - `enum ValueFamily { Scalar, Dictionary, PathList, Array, ... }`
   - `enum FamilyMember<'a> { Dense(&'a Value), Sparse(&'a Value), Unsupported }`
3. Move sparse-family operations behind one internal interface.
   Example responsibilities:
   - `can_yield(opinion) -> bool`
   - `compose_over(strong, weak, property_type) -> Option<Value>`
   - `materialize(value, property_type) -> Option<Value>`
4. Keep `Stage` orchestration-only.
   `Stage` should:
   - linearize opinions
   - fetch schema fallback and property typing
   - delegate family composition to `value_resolution`
   - package provenance

## Concrete Refactor Plan

### Phase 1: Isolate the current array resolver

Move the array-specific helpers out of `stage.rs` into
`value_resolution.rs` without changing behavior.

That yields:

- `resolve_sparse_family_chain(...)`
- `resolve_sparse_family_at_time_chain(...)`
- `ArrayFamily` as the first implementation

This step is mostly code motion plus API cleanup.

### Phase 2: Separate family selection from family implementation

Add an internal dispatcher that can inspect:

- authored `FieldValue`
- sampled `Value`
- schema fallback `FieldValue`

and answer:

- is this chain sparse-composable?
- if yes, which family owns it?

For now the dispatcher only returns `Array`.
That is still useful because it removes the array checks from `Stage`.

### Phase 3: Normalize dense and sparse family operations

Unify family behavior under one internal trait-like contract. This does not
need to be a public Rust trait; an enum plus match-based dispatch is enough and
keeps the core crate simpler.

The important invariant is:

- `compose_over(strong, weak)` must be associative within a family

That preserves flattening semantics.

### Phase 4: Generalize time-sampled family folding

Today the algorithm is:

- walk strongest-to-weakest opinions
- sample each time-sampled opinion at the query time
- filter to arrays
- compose arrays

That should become:

- walk strongest-to-weakest opinions
- sample each opinion into an optional family member
- feed those members into the family resolver

This makes the timeseries logic family-agnostic.

### Phase 5: Admit a second sparse family

The design is only proven once a second family can use the same framework.
Two reasonable candidates:

- path expressions, if or when they become distinct in the core model
- sparse dictionaries, if the repository grows that need

Until then, “generic” remains a design intention rather than an earned shape.

## What Should Not Change

These parts of the current implementation are correct and should remain:

- sparse opinions are authored values, not out-of-band resolver state
- resolved public values stay dense
- property typing is preserved close to authored fields
- schema fallback participates as the weakest dense seed for sparse families

## Practical Reading

- Runnable example:
  `layerstack_examples/examples/sparse_array_edits.rs`
- Current array-specific resolver:
  `layerstack/src/stage.rs`
- Sparse edit kernel:
  `layerstack/src/array_edit.rs`
