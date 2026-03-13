---
id: lay-v4vy
status: closed
deps: []
links: []
created: 2026-03-13T18:07:32Z
type: feature
priority: 1
assignee: Bruce Mitchener
parent: lay-c22s
tags: [data-model, composition, spec-alignment]
---
# Specifier resolution (def/over/class)

Implement specifier field resolution per §12.2.1 and §7.6 (specifier field definition). The specifier field determines whether a prim is concretely defining (def), abstractly defining (class), or non-defining (over). Value resolution for specifier has special rules: 'undefining' means all contributing opinions are 'over'; 'abstractly defining' means the strongest defining opinion is 'class'; 'concretely defining' means the strongest defining opinion is 'def'. This affects stage population queries — the 'defined' predicate (§11.5) requires the prim and all ancestors to have a defining specifier.

## Design

Add specifier to PrimSpec (likely as an enum field rather than a metadata field). During stage population and value resolution, resolve the specifier per the special rules in §12.2.1. Stage queries for 'active, loaded, defined, not abstract' use resolved specifier. The specifier is stored as a 32-bit int in binary (§16.3.10.27): def=0, over=1, class=2.

## Acceptance Criteria

Prims with only 'over' specs resolve as undefining. Class specs resolve as abstractly defining. Def specs resolve as concretely defining. Stage traversal correctly filters by defined/abstract predicates.

