# layerstack

`layerstack` is a small, domain-neutral composition kernel inspired by the OpenUSD Core
composition model.

The purpose of this repository is to build an AOUSD-aligned core (composition, population,
value resolution) and to keep it honest via a growing, fixture-driven conformance harness.

## What it does

- Layer stacks with deterministic strength ordering
- Stage population (composed prim tree)
- Value resolution (scalar + ListOp)
- Minimal v0.1 arcs: variants + references
- Provenance-friendly resolution APIs

## Repository layout

- `layerstack/`: the core crate (no_std by default; `alloc`-only APIs where practical)
- `layerstack_conformance/`: a std-only test harness that loads AOUSD supplemental fixtures and
  asserts behavior against `pcp.json` expectations
- `layerstack_examples/`: runnable examples using the `layerstack` API
- `docs/`: focused design notes for active architecture slices
- `core-spec-supplemental-release_dec2025/`: upstream AOUSD supplemental test corpus (vendored)
- `specs/`: normative spec PDFs used as implementation references

## What it does not do

- Rendering, layout, hit-testing
- Domain schemas or UI/canvas semantics
- USD file I/O

## Conformance (why the extra code is here)

`layerstack_conformance` exists to keep `layerstack` aligned with the AOUSD Core spec by running
composition fixtures and comparing results to the upstream `pcp.json` outputs (prim stacks,
property stacks, child ordering, etc).

This harness uses a deliberately minimal USDA reader for fixture inputs; it is not a general USD
file parser, and `layerstack` itself remains file-format agnostic.

## Status

Early development.

## Commands

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features --offline
```

## Sparse Array Edits

- Runnable example:
  `cargo run -p layerstack_examples --example sparse_array_edits`
- Follow-on architecture note:
  `docs/generic-sparse-composition.md`

## Specs

- AOUSD Core Spec (reference): `specs/aousd_core_spec_1.0.1_2025-12-12.pdf`
