# Architecture and authoring contracts

This note describes the boundaries and invariants that new workflows must preserve.
For API onboarding, start with the [workspace guide](../README.md#getting-started).
Compatibility evidence has a separate [validation contract](compatibility-and-validation.md).

## Ownership

| Owner | Responsibility |
| --- | --- |
| `opinionated` | Fold already ordered contributions; report how inputs participated; plan temporal folds without materializing host values |
| `layerstack` | Authored specs, composition strength, namespace and time mappings, value queries, edit targets and atomic transactions |
| Format crates | Decode and encode their supported representations, preserving typed data and reporting format failures |
| `layerstack_schemas` | Schema definitions, typed authoring/views and domain computations such as transforms, bounds and shading-source discovery |
| `layerstack_io` | Source identities, dependency loading, package anchors, retained bytes and explicit storage publication |
| Application | Edit intent, transport policy, budgets, acceptance of partial results, scheduling and renderer/execution ownership |

The generic kernel receives strength order; it does not discover USD arcs or load
assets. USD determines source participation, fallbacks and time mapping. Temporal
resolution composes the selected contributions at each bound before interpolating
the resulting values. Interpolating each source first and then folding can change
the answer. See [sparse composition](generic-sparse-composition.md) and
[opinionated's temporal guide](../opinionated/README.md).

Source provenance identifies which authored inputs contributed. Authorship history
records who changed them and why. Computational dependencies identify inputs to a
derived result. These are separate kinds of evidence; a property explanation does
not supply an edit history or execute a shading graph.

## A complete authoring workflow

Consider two scene occurrences referencing one asset. A local override on the first
occurrence should leave the second unchanged; editing the shared asset should
affect both unless stronger opinions mask it. The application chooses that intent.
The winning source alone cannot choose the appropriate writable destination.

1. Inspect the composed property and source explanation, including variants,
   namespace mapping and time offsets.
2. Choose an `EditTarget` for the intended layer, variant or composition node.
   A stage path and frame may map to a different spec path and source frame.
3. Build a `Transaction` with the required generation or authored-value guards.
   Apply it through `LiveStage` to update composition and obtain its inverse.
4. Check the requested visible result and the other occurrence separately. A
   successful authored write can remain masked by stronger opinions.
5. Apply the inverse when undoing, then check inheritance is visible again.
6. Save the intended authored layers, reopen through the destination's actual
   asset-resolution context, and verify the same behavior.

The mapped-time example in [the edit module](../layerstack/src/edit/mod.rs) and
`cargo run -p layerstack_examples --example tutorial_stage_io` exercise the
existing primitives. This sequence is an application validation pattern; there is
no general API that certifies arbitrary visible postconditions automatically.

Undo restores authored slots, including absence. Writing the old effective value
as a new local opinion would conceal inheritance and produce different future
behavior. Inverses are guarded against overwriting intervening changes to the
same slots. Authored equality also preserves floating-point representation,
including signed zero and NaN payloads.

Namespace edits use `NamespaceEdit` preparation and its affected-layer/prim preview.
Dependent-stage coordination has explicit coverage requirements; a masked or
unloaded region cannot silently count as checked. See the
[coordinated namespace tests](../layerstack_conformance/tests/conformance/namespace_coordinated.rs).

## Evidence and invalidation

An authored mutation, a changed effective answer and invalidation are distinct.
`Changes` reports exact created/removed paths, subtree resync roots and info-only
paths. An info-only notice does not assert a different resolved value. A missing
property-field inventory means unknown, rather than unchanged.

[Stage notices](stage-notices.md) expose completed updates and bounded replay.
[Retained queries](retained-queries.md) separately report evaluation, answer changes,
dependency changes and provenance changes. A source switch can preserve a value;
an unrelated authored change can require no evaluation.

Coverage belongs in any broader inspection contract: distinguish checked affected
inputs, checked unaffected inputs and inputs not examined. Budget exhaustion or a
missing source is incomplete evidence. A complete clip plan may instead confirm
a real value gap; completeness does not imply a value exists.

`TokenId`, `PathId` and allocated layer identities are store-local. Durable
application records need host source identifiers and textual paths plus the
relevant source/context revision. Rejected reload candidates have an independent
ID domain; candidate-only IDs must not escape into the accepted document.

## Namespace and time mapping laws

Composition and authoring must share the same mapping semantics:

- Sequential mapping must agree with composed mapping, including unmappable
  domains, root identity, exclusions and relocates.
- Internal arcs, external arcs and class arcs retain their own identity domains.
  Namespace mapping cannot be reduced to replacing a string prefix.
- Apply each layer/time offset once, in the same direction as its source mapping.
- Preserve repeated source occurrences and their order. A layer identifier alone
  is insufficient to identify a contribution.
- List edits act in their source occurrence and composition context. Arc expansion
  order cannot stand in for strength order. Suppressed targets must not leak
  diagnostics as though they still participate.

Small counterexamples live in the [conformance tests](../layerstack_conformance/tests/),
including `edit_target_maps.rs`, `edit_targets.rs` and `composition_strict.rs`.

## Save, flatten, package and publish

| Operation | Intended result |
| --- | --- |
| Save an authored layer | Persist its supported authored opinions and source structure |
| Flatten a stage | Materialize composed scene description under the flattening options and supported query semantics |
| Flatten a layer stack | Consolidate local layers while preserving external composition arcs |
| Prepare/export a package | Inventory and localize dependencies, retaining explicit external-dependency policy |

These operations have different fidelity and dependency scopes. A package is
verified after resolving its member-relative dependencies and composing its root,
as well as checking archive structure. Repeated basenames in distinct members must
not share a resolution cache entry accidentally.

[`StageDocument`](../layerstack_io/README.md) makes reload and package preparation
explicit. Reload candidates can be inspected and rejected before publication.
Filesystem saves replace one file through a synchronized sibling candidate;
that guarantee does not make a multi-layer save atomic. Platform metadata limits
are documented by the I/O crate.

Saving currently retains warmed decoded arrays. Explicit reload replaces sources,
recomposes and publishes a resync; it is not storage-only rebinding. Immutable
encoded owners and numeric buffers can remain alive through external snapshots.
Any future compaction must preserve those snapshots and distinguish storage
publication from authored generations and scene notices. Writer-table reuse alone
does not release decoded arrays. See the measured
[save ownership costs](../wind_tunnel/README.md).
