---
id: lay-889c
status: closed
deps: [lay-n7r4]
links: []
created: 2026-03-13T18:06:42Z
type: feature
priority: 2
assignee: Bruce Mitchener
parent: lay-avtk
tags: [value-resolution, composition, spec-alignment]
---
# Layer offset and scale

Implement layer offset and scale for time retiming (§12.3.2.1). References and sublayers can specify offset and scale values that remap time when querying time samples from referenced/sublayered content. The formula is: mappedTime = (queryTime * scale) + offset. Scale must be positive and non-zero. The LayerOffset type is defined in §16.3.10.20 as two doubles (offset, scale). This affects both timeSamples and spline evaluation.

## Design

Add LayerOffset to the Reference struct and sublayer entries. During time-based value resolution, when crossing a reference or sublayer boundary, apply the offset/scale transform to the query time. The transform composes when references are nested. Negative scale is explicitly problematic (§12.3.2.1) and should be rejected or warned.

## Acceptance Criteria

BasicTimeOffset_root and ReferenceListOpsWithOffsets_root conformance fixtures pass. Time queries through references with offsets return correctly retimed values. Nested offsets compose correctly.

