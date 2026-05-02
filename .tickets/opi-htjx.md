---
id: opi-htjx
status: open
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

