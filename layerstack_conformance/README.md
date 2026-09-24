# layerstack_conformance

Conformance harness and golden tests for `layerstack`.

This crate is allowed to use `std` and external test-only dependencies. Core
composition logic remains in the `layerstack` crate.

## Inputs

- AOUSD core spec PDF reference: `specs/aousd_core_spec_1.0.1_2025-12-12.pdf`
- AOUSD supplemental test materials: `core-spec-supplemental-release_dec2025/`

## Exporter interoperability (optional)

`scripts/export_interop.sh [OUT_DIR]` writes the exporter fixtures from
`src/export_fixtures.rs` and checks them with OpenUSD's `usdcat` and
`usdchecker` (default and `--arkit` validators) and Python's `zipfile`
(`scripts/check_usdz_layout.py`), including negative controls. It records tool
versions and the selected validator rules in `OUT_DIR/report.txt`. It is not
run by CI; `tests/export_interop.rs` runs the `usdcat`/`usdchecker` subset when
those tools are on `PATH` and skips otherwise.
