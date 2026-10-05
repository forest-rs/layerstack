---
id: li-w8jk
status: closed
deps: []
links: []
created: 2026-10-05T03:52:43Z
type: bug
priority: 2
assignee: Bruce Mitchener
---
# Refresh expression dependencies during prepared root reload

A freshly reloaded root retains old evaluated expression bindings, so its dependency is not refreshed and dirty protection is skipped.

## Design

Hide published bindings for candidate-replaced anchor layers during expression discovery. Preserve unchanged/session bindings and rejected document state.

## Acceptance Criteria

Expression dependencies read updated values once, preserve IDs, reject required dirty sources and missing dependencies, and recover after rejection.

