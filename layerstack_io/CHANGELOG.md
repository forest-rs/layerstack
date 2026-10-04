# Changelog

## Unreleased

Initial release.

- Host-owned storage and retained stage documents for loading USDA, USDC and USDZ.
- Explicit source tracking, dirty-layer saves, session-layer saves, exports and
  reload policies that protect unsaved edits.
- Filesystem transport with caller-selected roots and search paths behind the
  default `std` feature; custom storage works with `no_std` + `alloc`.
