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

