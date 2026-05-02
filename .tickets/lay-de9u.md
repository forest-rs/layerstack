---
id: lay-de9u
status: open
deps: []
links: []
created: 2026-07-02T04:44:33Z
type: task
priority: 2
assignee: Bruce Mitchener
---
# Adopt opinionated ListOp in layerstack

layerstack::listop and opinionated::ListOp now have identical semantics (explicit makes other edits spurious, AOUSD Core 12.4) but are separate types. Replace layerstack's ListOp/resolve_list_chain with a dependency on opinionated (or a re-export) so there is one list-edit kernel in the workspace.

## Acceptance Criteria

layerstack uses opinionated's ListOp; conformance suite still passes; no duplicated list-op code remains.

