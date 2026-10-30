# Changelog

## Unreleased

Initial release.

- WebAssembly bindings for `@forest-rs/layerstack`: open a stage from the bytes
  of its root layer, with the host supplying each asset the stage asks for.
- Prim, attribute, relationship, metadata and value queries, value
  explanations, and composed numeric arrays viewed in place in the module's
  memory.
- Layer muting, which is stage state and not an undoable edit, heard in the
  change records with the layers muted and unmuted: by layer or by
  identifier, before the stage reads a layer, and several layers in one
  request.
- Material binding resolution (direct, collection, inherited, by purpose),
  material face subsets with their family's validation, material terminal
  sources, shader inputs and outputs with their connections, value sources
  and `sdrMetadata`, geometry subsets and coordinate-system bindings.
- Schema-shaped edits: bind and unbind materials, set shader inputs,
  connect and disconnect them, set a base material, create geometry and
  material subsets and set a family's type, bind, block and clear
  coordinate systems, and set `sdrMetadata`, in the same atomic batch as
  the generic edits.
- Atomic edits with undo and change records, saves of dirty layers, and
  exports of a layer, the flattened stage or a USDZ package.
- An `rlib` for crates that link the bindings into a module of their own.
