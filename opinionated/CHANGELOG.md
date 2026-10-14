<!-- Instructions

This changelog follows the patterns described here: <https://keepachangelog.com/en/>.

Subheadings to categorize changes are `added, changed, deprecated, removed, fixed, security`.

-->

# Changelog

The latest published Opinionated release is [0.1.0](#010-2026-10-14) which was released on 2026-10-14.
You can find its changes [documented below](#010-2026-10-14).

## [Unreleased]

## [0.1.0][] (2026-10-14)

This release has an [MSRV][] of 1.89.

This is the initial release.

### Added

- Layered sparse opinion storage over typed addresses, with scalar, unique-list,
  and shallow dictionary resolution.
- Storage-independent resolution for host-defined value and edit families,
  recursive dictionary combination, and sparse array edit programs.
- Temporal composition planning over host-defined time keys and values.
- Provenance, opinion-stack inspection, and explanation reports.

The crate uses `no_std` with `alloc` and has no dependencies or feature flags.

[Unreleased]: https://github.com/forest-rs/layerstack/compare/opinionated-0.1.0...HEAD
[0.1.0]: https://github.com/forest-rs/layerstack/releases/tag/opinionated-0.1.0
[MSRV]: README.md#installation
