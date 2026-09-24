---
id: opi-yib9
status: closed
deps: []
links: []
created: 2026-07-02T04:44:23Z
type: task
priority: 2
assignee: Bruce Mitchener
---
# Decide nested dictionary merge semantics

combine_dictionary_chain merges shallowly by key; USD merges dictionaries recursively. Decide whether opinionated grows recursive merge (requires V to expose nested dictionaries somehow) or documents shallow-only as a permanent contract.

## Acceptance Criteria

Decision recorded; either recursive merge implemented and tested, or README/docs state shallow-only as a stable contract.


## Notes

**2026-09-24T15:52:02Z**

Decided: opinionated owns recursive dictionary combination (combine_dictionaries / combine_dictionary_chain over a host DictionaryAdapter, AOUSD Core §6.6.2.1), folding strongest-first as OpenUSD does (§4.2; MetadataValueComposer::ConsumeAuthored, confirmed with usdcat on {s:{a:1}},{s:0},{s:{b:2}} -> {s:{a:1,b:2}}). ShallowOverlay is the separate named policy, used by the OpinionOp enum API whose V is opaque. Remaining: layerstack delegates its dictionary resolution.

**2026-09-24T15:53:52Z**

Done: layerstack implements DictionaryAdapter for Value and delegates combine_dictionaries/combine_dictionary_chain and Stage dictionary resolution to opinionated. Pinned by opinionated/tests/dictionary_combine.rs and layerstack/tests/conformance.rs (three-layer conflict matching usdcat, key order, blocks, schema fallback seed, parity with an independent reference fold up to key order).
