---
id: li-y1k9
status: closed
deps: []
links: []
created: 2026-10-05T03:20:56Z
type: bug
priority: 2
assignee: Bruce Mitchener
---
# Prepare reload from the freshly reachable source closure

A renderer reload selecting prior used sources can be blocked by obsolete A, even when the changed root removed A, if A now introduces missing C. Root-only preparation currently reuses cached resident dependencies instead of refreshing them.

## Design

Expose an explicit current-closure prepared reload: invalidate catalog freshness only in the candidate and load the current authored root file dependency closure once, retaining IDs and dirty protection. Do not force old unused package members.

## Acceptance Criteria

Removed A with newly missing C recovers; required missing dependencies reject without publication; retained dependencies refresh; package removal and dirty protection pass.

