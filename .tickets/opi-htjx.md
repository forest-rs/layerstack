---
id: opi-htjx
status: closed
deps: []
links: [opi-s92f]
created: 2026-07-02T04:44:23Z
type: feature
priority: 2
assignee: Bruce Mitchener
---
# Support fallback seeds in composer resolution

resolve_list_chain accepts a fallback slice but SparseComposer::resolve hardcodes an empty seed. Callers need a way to supply a schema-fallback-like weakest value (dense seed) that participates as the weakest opinion, matching how layerstack seeds sparse resolution with schema fallbacks.

## Acceptance Criteria

A caller can resolve a list/dictionary field over a supplied fallback without modeling the fallback as an extra layer.


## Notes

**2026-07-02T17:29:21Z**

Implemented as resolve_ordered_chain_with_fallback + SparseComposer::resolve_with_fallback (opinionated/src/lib.rs), sharing one fold_ordered_chain with the existing entry point. Semantics: the fallback is a dense SEED, not an opinion - list chains fold over the fallback list, dictionary chains combine over the fallback entries, scalar chains never consult it, shape mismatches are ignored, an empty chain stays Absent (a seed has no provenance), a strongest block stays Blocked, and a weaker block cuts weaker opinions while stronger edits still fold over the seed (matching resolve_family_chain and layerstack's ArrayFamily). Covered by opinionated/tests/fallback_seeds.rs (8 tests).
