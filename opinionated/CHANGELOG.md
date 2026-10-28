<!-- Instructions

This changelog follows the patterns described here: <https://keepachangelog.com/en/>.

Subheadings to categorize changes are `added, changed, deprecated, removed, fixed, security`.

-->

# Changelog

The latest published Opinionated release is [0.2.0](#020-2026-10-28) which was released on 2026-10-28.
You can find its changes [documented below](#020-2026-10-28).

## [Unreleased]

## [0.2.0][] (2026-10-28)

This release has an [MSRV][] of 1.89.

### Added

- `ListOp::compose_over` combines a stronger list op with a weaker one into
  the single op that edits any list as applying the two in turn does. It
  returns `None` when an add or a reorder makes the result depend on the list
  being edited, as OpenUSD's `SdfListOp::ApplyOperations(const SdfListOp&)`
  does. ([#223][] by [@waywardmonkeys][])
- A `hash-tables` feature, on by default, which depends on `hashbrown` to hash
  long lists so list-op edits take linear time. Disabling it removes the
  dependency, and the crate again has none: every lookup then scans, with the
  same results, but an edit naming `k` items over a list of `n` items takes
  `O(n * k)` time. The API is the same with or without the feature.
  ([#223][] by [@waywardmonkeys][])

### Changed

- Breaking: list items must implement `Hash`, with or without the
  `hash-tables` feature. The bound is added to `ListOp::apply_to`,
  `ListOp::apply_in_place`, `resolve_list_chain`, `resolve_list_chain_report`,
  `apply_list_order`, `resolve_ordered_chain`,
  `resolve_ordered_chain_with_fallback`, `resolve_ordered_chain_report`,
  `resolve_ordered_chain_with_fallback_report`, the `SparseComposer` methods
  `resolve`, `resolve_with_fallback`, `explain` and `explain_with_fallback`,
  and `ListFamily`'s `OpinionFamily` implementation.
  ([#223][] by [@waywardmonkeys][])
- `ListOp::apply_to`, `ListOp::apply_in_place`, `apply_list_order` and the
  list resolvers built on them take time linear in the lengths of the list
  and of its authored items, instead of quadratic. Short lists are still
  scanned without allocating; with `hash-tables`, an edit hashes once
  scanning would take more than 1,024 comparisons. Results are unchanged.
  ([#223][] by [@waywardmonkeys][])
- Array edit composition is checked against OpenUSD 26.11's
  `testComposeFillLiterals` vectors, which correct how OpenUSD rebases a
  stronger `minsize` or `resize` fill over a weaker edit with literals.
  `ArrayEditOp::MinSizeFill` and `ArrayEditOp::ResizeFill` hold their fill
  element, so the results already agreed and are unchanged.
  ([#237][] by [@waywardmonkeys][])

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

[Unreleased]: https://github.com/forest-rs/layerstack/compare/opinionated-0.2.0...HEAD
[0.2.0]: https://github.com/forest-rs/layerstack/compare/opinionated-0.1.0...opinionated-0.2.0
[0.1.0]: https://github.com/forest-rs/layerstack/releases/tag/opinionated-0.1.0
[MSRV]: README.md#installation

[#223]: https://github.com/forest-rs/layerstack/pull/223
[#237]: https://github.com/forest-rs/layerstack/pull/237

[@waywardmonkeys]: https://github.com/waywardmonkeys
