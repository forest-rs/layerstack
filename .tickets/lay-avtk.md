---
id: lay-avtk
status: open
deps: []
links: []
created: 2026-03-13T18:04:27Z
type: epic
priority: 1
assignee: Bruce Mitchener
tags: [value-resolution, spec-alignment]
---
# Value Resolution

Implement the full value resolution pipeline per §12. Currently the stage resolves scalar fields by returning the strongest opinion and composes list ops. Missing: time-varying resolution (timeSamples, splines, value clips), interpolation methods, value blocking, layer offset/scale, fallback values from schema definitions, and dictionary combining.

## Design

Value resolution sits on top of composition. The Stage API needs to accept a time parameter. Resolution order per §12.3: timeSamples > spline > default > clips, checked per-spec in strength order. Interpolation (§12.5) is a separable concern. Value clips (§12.3.4) are the most complex subsystem with manifest files, active/times metadata, and template expansion.

## Acceptance Criteria

Upstream value_resolution test fixtures pass. Time-varying queries return correct interpolated values. Value blocking correctly skips weaker opinions and falls back.


## Notes

**2026-03-14T04:41:59Z**

Interpolation methods (lay-2b56) closed: Held + Linear interpolation implemented with TimeSamples support in 4b98875.

**2026-03-14T04:51:33Z**

TimeSamples resolution (lay-n7r4) closed: timeSamples parsing + FieldValue::TimeSamples + resolve_value_at_time in 0cb2835.

**2026-03-17T13:35:05Z**

Dictionary combining (lay-zjzv) closed: Value::Dictionary + recursive merge + emit pipeline + stage resolution in ae08ad9.
