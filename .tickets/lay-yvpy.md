---
id: lay-yvpy
status: open
deps: [opi-s92f, lay-de9u]
links: []
created: 2026-07-02T04:44:33Z
type: feature
priority: 2
assignee: Bruce Mitchener
---
# Delegate chain value resolution to opinionated kernel

Long-term goal: layerstack's value_resolution.rs delegates scalar/list/dictionary strong-over-weak folding to opinionated's resolve_ordered_chain, with layerstack-specific families (sparse array edits, time samples) plugging in through the extensibility seam. This keeps opinionated honest and drives layerstack toward a general sparse-composition kernel.

## Acceptance Criteria

layerstack's Stage resolution routes non-array families through opinionated with no behavior change in the conformance suite.


## Notes

**2026-07-02T17:09:07Z**

ArrayFamily implemented in layerstack/src/value_resolution.rs over opinionated's OpinionFamily kernel; the array-specific fold (OpinionFoldStep/fold_sparse_members/compose_over/materialize) is replaced by chain construction + resolve_family_chain. Design notes realized: at-time path samples opinions into member Values before classify; ArrayFamily carries PropertyType (typed apply) and the schema fallback (through seed(), materializing ArrayEdit fallbacks over the empty array); SparseResolveResult interface to Stage unchanged. One deliberate semantic change, per kernel design: a block cutting the chain no longer suppresses STRONGER sparse edits - they materialize over the seed (new unit test stronger_edits_materialize_over_seed_when_block_cuts_chain). Conformance suite, sparse_array_edits example, fmt/clippy/tests/no_std all green. Follow-up unblocked: opi-htjx (caller-supplied seed override).
