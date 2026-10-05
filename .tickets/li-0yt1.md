---
id: li-0yt1
status: closed
deps: []
links: []
created: 2026-10-05T03:53:43Z
type: bug
priority: 2
assignee: Bruce Mitchener
---
# Make sparse resource regression portable across wasm targets

Explicit drop(File) triggers drop_non_drop on wasm32-unknown-unknown, and std::env::temp_dir panics under the WASI CI runner.

## Design

Close the sparse test handle with lexical scope; place the isolated fixture in the runner-accessible working directory and remove it after the check.

## Acceptance Criteria

Native sparse test still rejects before payload read; wasm32 Clippy and WASI I/O tests pass.

