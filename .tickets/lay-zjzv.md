---
id: lay-zjzv
status: closed
deps: [lay-rsdo]
links: []
created: 2026-03-13T18:06:52Z
type: feature
priority: 2
assignee: Bruce Mitchener
parent: lay-avtk
tags: [value-resolution, data-model, spec-alignment]
---
# Dictionary combining

Implement dictionary value combining per §6.6.2.1 and §12.2.5. When multiple specs author dictionary-valued fields (e.g., customData), the dictionaries are recursively merged rather than using strongest-wins. The combining rules: stronger non-dict values win, matching keys where both values are dicts recurse, weaker-only keys are preserved. This is a closed, associative operation on the domain of dictionaries.

## Design

Add a Dictionary variant to Value or FieldValue that holds a map of string→Value. During value resolution for dictionary-typed fields, compose all opinions using the recursive combining algorithm from §6.6.2.1 instead of returning the strongest opinion. The formal definition uses set-theoretic notation: S ∪ W with special handling for nested dicts.

## Acceptance Criteria

customData dictionaries from multiple layers merge correctly. Nested dictionary combining works recursively. Non-dict values at the same key use strongest-wins. Upstream conformance tests involving customData pass.

