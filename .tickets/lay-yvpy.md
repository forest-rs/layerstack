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

