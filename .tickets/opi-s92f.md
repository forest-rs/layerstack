---
id: opi-s92f
status: open
deps: []
links: [opi-htjx]
created: 2026-07-02T04:44:23Z
type: feature
priority: 2
assignee: Bruce Mitchener
---
# Design extensibility seam for opinion families

OpinionOp is a closed enum, so domains cannot add sparse value families (e.g. layerstack's sparse array edits or time-sampled values) without forking the crate. Decide between a family trait, a Custom op variant folded by the caller, or family-generic fold hooks. This is the gating design question for layerstack delegating chain resolution to opinionated.

## Design

OpinionKind already acts as a family discriminator; docs/generic-sparse-composition.md describes the same problem from layerstack's side. The compose_over(strong, weak) operation must stay associative within a family to preserve flattening semantics.

## Acceptance Criteria

A second, out-of-crate value family can participate in resolve/explain without modifying opinionated's enums.

