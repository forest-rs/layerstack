# Changelog

## Unreleased

Initial release.

- Numeric schema array getters and `Primvar::indices` retain `Arc<Vec<T>>`
  storage. Migration: borrow with `as_slice()` / `iter()` or explicitly copy
  with `as_ref().clone()` when a mutable `Vec<T>` is required. Half and matrix
  representation conversions retain vector return types. Numeric shader-node
  array defaults also return shared owners; slice setters remain compatible.
- Numeric edit handles provide `_owned` and `_shared` setters, including time
  samples, and primvar index setters support the same transfers. Conversion
  helpers document borrowed, shared and explicitly materialized reads.
