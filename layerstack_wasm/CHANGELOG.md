# Changelog

## Unreleased

Initial release.

- WebAssembly bindings for `@forest-rs/layerstack`: open a stage from the bytes
  of its root layer, with the host supplying each asset the stage asks for.
- Prim, attribute, relationship, metadata and value queries, value
  explanations, and composed numeric arrays viewed in place in the module's
  memory.
- Atomic edits with undo and change records, saves of dirty layers, and
  exports of a layer, the flattened stage or a USDZ package.
- An `rlib` for crates that link the bindings into a module of their own.
