---
id: lay-y8q1
status: closed
deps: [lay-rsdo]
links: [lay-kl8k]
created: 2026-03-13T18:08:22Z
type: feature
priority: 3
assignee: Bruce Mitchener
parent: lay-uc3n
tags: [schemas, spec-alignment]
---
# Schema registry and typed/API schemas

Implement the schema system per §13. Typed schemas (IsA, §13.3) define prim types with a single-inheritance hierarchy and built-in properties with fallback values. API schemas (HasA, §13.3) are mix-in schemas that add additional properties and behaviors. Schema definitions include: type name, parent schema (for typed), applied API schemas, property definitions with types and fallback values, and schema-level metadata. Schema ordering (§13.3) governs how multiple schemas compose on a single prim.

## Design

Create a SchemaRegistry that maps schema type tokens to their definitions (property list, fallback values, parent schema, API schemas). During value resolution, when no authored opinion exists for a schema-defined property, return the fallback value from the schema definition. IsA schemas form a single inheritance chain. HasA schemas can be multiply applied. The schema ordering for a prim definition follows §13.3 rules.

## Acceptance Criteria

Schema-defined fallback values returned when no opinion authored. IsA inheritance works. API schemas can be applied and provide their properties. Schema ordering matches §13.3.

