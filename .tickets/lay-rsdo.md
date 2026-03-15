---
id: lay-rsdo
status: closed
deps: []
links: []
created: 2026-03-13T18:07:02Z
type: feature
priority: 2
assignee: Bruce Mitchener
parent: lay-c22s
tags: [data-model, spec-alignment]
---
# Expand Value enum with full scalar types

Expand the Value enum to cover all scalar types from §6.2. Currently: Bool, Int(i64), Float(f64), String, Token, Opaque. Missing scalar types: half (f16), uchar (u8), int (i32 — currently using i64), uint (u32), int64 (i64 — have this), uint64 (u64), timecode (f64 with semantic meaning), asset (string with resolution semantics, distinct from plain string). The asset type is particularly important as it's used for layer references and texture paths.

## Design

Consider whether to add individual variants (UInt(u32), Half(f16), etc.) or use a more compact representation. The half type may need the half crate or a manual f16 implementation per §16.3.10.8 (IEEE 754-2008). Asset should be a distinct variant from String since assets undergo variable substitution and resolution (§6.2). Keep no_std compatible.

## Acceptance Criteria

All §6.2 scalar types can be represented in the Value enum. Asset values are distinct from string values. Round-trip preservation of types through composition.

