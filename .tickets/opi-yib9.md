---
id: opi-yib9
status: open
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
