---
id: lay-yvpy
status: closed
deps: [opi-s92f, lay-de9u]
links: []
created: 2026-07-02T04:44:33Z
type: feature
priority: 2
assignee: Bruce Mitchener
---
# Delegate sparse array chain folding to opinionated kernel

Delegate layerstack's sparse (array-edit) chain folding in value_resolution.rs to opinionated's family kernel (resolve_family_chain), with layerstack-specific concerns (time sampling, typed materialization, schema fallback seeds) plugging in through the OpinionFamily seam. This keeps opinionated honest with a real consumer.

Scope revised 2026-09-24: the original goal routed scalar/list/dictionary resolution through this same family kernel. That is not the right shape for every kind, so it is not deferred here either: list ops already share opinionated's ListOp directly; dictionaries are delegated to opinionated's dedicated strongest-first recursive combiner via a DictionaryAdapter (opi-yib9), because recursive combining is not associative and the family kernel applies edits weakest-first; scalar strongest-wins gains nothing from the indirection. A family should move onto the kernel only when that removes code or adds capability (see docs/generic-sparse-composition.md).

## Acceptance Criteria

layerstack's sparse array resolution (default and at-time queries, including blocks and schema fallback seeds) folds through opinionated's family kernel, with the conformance suite and sparse_array_edits example passing. Any behavior change is intentional, spec-referenced, and tested.


## Notes

**2026-07-02T17:09:07Z**

ArrayFamily implemented in layerstack/src/value_resolution.rs over opinionated's OpinionFamily kernel; the array-specific fold (OpinionFoldStep/fold_sparse_members/compose_over/materialize) is replaced by chain construction + resolve_family_chain. Design notes realized: at-time path samples opinions into member Values before classify; ArrayFamily carries PropertyType (typed apply) and the schema fallback (through seed(), materializing ArrayEdit fallbacks over the empty array); SparseResolveResult interface to Stage unchanged. One deliberate semantic change, per kernel design: a block cutting the chain no longer suppresses STRONGER sparse edits - they materialize over the seed (new unit test stronger_edits_materialize_over_seed_when_block_cuts_chain). Conformance suite, sparse_array_edits example, fmt/clippy/tests/no_std all green. Follow-up unblocked: opi-htjx (caller-supplied seed override).

**2026-09-24T15:35:01Z**

Closed against the revised acceptance. Array delegation landed in b2a3269; follow-ups on this branch: the adapter folds Opinions lazily and stops at the first dense value/block (no hidden opinion sampled/cloned), sampled Value::Blocked now blocks (AOUSD Core §12.3.6), the edit-over-block semantics are pinned with Stage-level tests and spec/proposal citations, and docs/generic-sparse-composition.md describes the delegated shape and remaining proposal gaps. Other value kinds are not routed through the family kernel: dictionaries delegate to opinionated's recursive combiner instead (opi-yib9), list ops share ListOp, scalars stay in Stage (see description).
