# AGENTS.md

This repository is maintained with help from AI coding agents (e.g. Codex/ChatGPT).
This file defines how to make changes, what “done” means, and the project defaults we enforce.

## North Star

- Keep core crates small, predictable, and long-lived.
- Prefer simple, explicit designs over clever ones.
- Avoid dependency creep; keep compile times and surface area under control.
- Optimize for long-term architecture over short-term compatibility; it’s OK to break callers to get the right core shape.

## Non-negotiables (Definition of Done)

- `cargo fmt` passes.
- `cargo clippy` passes (`-D warnings`).
- Public APIs are documented (types/functions; public fields/variants where it matters).
- Semantics-changing code includes AOUSD spec references (section-level is OK; keep close to the implementation).
- Tests updated/added when behavior changes.
- Examples/benchmarks live in separate top-level workspace crates (no extra dev-deps in core crates).

Suggested commands:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

## Rust workspace expectations

- MSRV is set in `Cargo.toml` (`rust-version = "1.88"`); keep it compatible.
- Follow workspace lint policy (notably: `unsafe_code = "deny"` and `missing_docs = "warn"`).

## `no_std` policy (core crates)

- Default assumption for foundational crates: `#![no_std]` whenever practical (use `extern crate alloc` when needed).
- Keep `std` behind an explicit `std` feature flag when required.
- Avoid `std` collections in `no_std` crates; use `hashbrown` (and `alloc` types) instead.

## Dependency policy

- Keep the incremental viz core “pure”: no direct dependencies on render backends (no `wgpu`, `vello`, `masonry`, etc.).
- Prefer small utilities already in the workspace deps (`hashbrown`, `smallvec`) over adding new crates.
- If introducing a new dependency, justify why it’s needed and which features are enabled.

## Tests, examples, benchmarks

- Unit tests live next to code; keep them deterministic.
- Examples and benchmarks live in separate top-level workspace crates so extra dependencies don’t appear as dev-dependencies of core crates.

## Tooling / workflow

- Prefer `rg` for code search.
- Keep diffs small and reviewable; preserve existing style unless improving consistency.
