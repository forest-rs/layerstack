## Forest Engineering Tenets

These tenets govern all forest-rs projects. They are non-negotiable.

1. **We Build to Endure.** Systems that are difficult to outgrow, difficult to entangle, easy to reason about, easy to measure. Optimize for structural strength, not short-term applause.
2. **Modularity Is Power.** Every subsystem: narrow responsibility, minimal dependency surface, replaceable internals, stable API. Monoliths are a last resort.
3. **Incrementalism Everywhere.** Full rebuilds are failure modes. Deltas over rewrites. Patches over full uploads. Caches over recomputation. Budgeted work over spikes.
4. **Introspection Is Non-Optional.** If we cannot measure it, we cannot improve it. Every system exposes: time (CPU + GPU), memory (live + fragmentation), work units, bandwidth. Diagnostics are architecture.
5. **Explicit Over Implicit.** No hidden state. No invisible scheduling. No accidental lifetime behavior. No magical performance characteristics. Predictability is a feature.
6. **Long-Term > Short-Term.** Clean structure over clever shortcuts. Extensibility over demo velocity. Architectural leverage over temporary wins.
7. **Replaceability Is a Constraint.** Major subsystems tolerate different backends, techniques, allocators, platforms. If something cannot be replaced, it must be small and contained.
8. **Calm Interfaces.** Internal complexity may be aggressive. Public APIs must be calm: boring, obvious, stable, intentional.
9. **No Sacred Subsystems.** Refactor without attachment. Remove complexity when possible. Evolve forward.

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

- MSRV is set in `Cargo.toml` (`rust-version = "1.89"`); keep it compatible.
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

## Tickets / Issue Tracking / Plans

This project uses `tk`, a CLI ticket system. Tickets live in `.tickets/` as markdown files and are committed alongside code. Run `tk help` for usage.

## Creating tickets

- Run `tk create` from within the owning crate directory (`crates/<crate>/`) so the ticket gets a crate-scoped prefix.
- Include `-d` (description), `--design`, and `--acceptance` when the detail is known.
- Use `tk link` and `tk dep` to connect related work.
