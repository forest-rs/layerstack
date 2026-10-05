---
id: li-gxez
status: closed
deps: []
links: []
created: 2026-10-05T03:20:56Z
type: bug
priority: 2
assignee: Bruce Mitchener
---
# Bound arbitrary resource reads before allocation

Layerstack consumers enforce image byte budgets, but read_asset_bytes copies complete storage/package bytes before callers can inspect their size. Oversized sources can exhaust memory first.

## Design

Add explicit byte/package limits and a fail-closed Storage bounded-read extension; check package member size before copying. Preserve unbounded methods for existing callers.

## Acceptance Criteria

Sparse oversized filesystem resource and oversized resident package member reject before payload allocation; candidate and document reads share the limit; existing APIs remain unchanged.

