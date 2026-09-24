# layerstack_conformance

Conformance harness and golden tests for `layerstack`.

This crate is allowed to use `std` and external test-only dependencies. Core
composition logic remains in the `layerstack` crate.

## Inputs

- AOUSD core spec PDF reference: `specs/aousd_core_spec_1.0.1_2025-12-12.pdf`
- AOUSD supplemental test materials: `core-spec-supplemental-release_dec2025/`

## Exporter interoperability (optional)

`scripts/export_interop.sh [OUT_DIR]` writes the exporter fixtures from
`src/export_fixtures.rs` (USDA, USDC, and generic and ARKit-profile USDZ) and
checks them with OpenUSD's `usdcat` and `usdchecker` (default and `--arkit`
validators) and Python's `zipfile` (`scripts/check_usdz_layout.py`, which also
checks the ARKit profile's single USDC root layer), including negative
controls. It compares `usdcat`'s text for each document's USDC and USDA.
Material fixtures are also checked for texture color spaces as `usdcat` reads
them and, when `usdrecord` is available, rendered through a camera wrapper
layer (`scripts/render_check.py`) in both profiles: the two-material cube must
show both materials' colors and the textured cube both texture hues; the
ARKit and generic plain cubes must render identically. It records tool
versions, the selected validator rules and the renders in `OUT_DIR`. It is not
run by CI; `tests/export_interop.rs` runs the `usdcat`/`usdchecker` subset and
the USDC differential (our USDC against OpenUSD's `usdcat -o` output for the
same USDA, compared structurally) when those tools are on `PATH` and skips
otherwise.
