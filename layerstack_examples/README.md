# layerstack_examples

Small runnable programs that demonstrate how to use the `layerstack` API, and the
`opinionated` kernels on their own.

## Run

- `cargo run -p layerstack_examples`
- `cargo run -p layerstack_examples --example minimal`
- `cargo run -p layerstack_examples --example sparse_array_edits`
- `cargo run -p layerstack_examples --example explain_value`
  (explains the values of a tree asset referenced into a grove: a local override, a sparse edit
  on its points from a sublayer, time samples through the reference's layer offset, combined
  `customData` and an inherited attribute)
- `cargo run -p layerstack_examples --example flatten`
  (flattens a grove of referenced trees, one retimed, with a sublayer's overrides, into a single
  USDA file with no composition arcs, as `usdcat --flatten` does, prints the report of what it
  wrote exactly and what it transformed, and verifies that the file composes the same values on
  its own)
- `cargo run -p layerstack_examples --example typed_schema`
  (authors a mesh, a sphere light and a collection through `layerstack_schemas`' edit handles in
  a base layer and a stronger shot layer, then reads them through the typed views: the strongest
  opinion, schema fallbacks, `allowedTokens` enums and inherited properties, the purpose and
  visibility the arm inherits, and the raw value with its provenance)
- `cargo run -p layerstack_examples --example world_transforms`
  (authors a small animated orrery through the transform op API, then prints the world
  transform, visibility and purpose of every `Gprim` at two time codes, through one `XformCache`
  per time code)
- `cargo run -p layerstack_examples --example light_rig_slots`
  (sparse edits to a typed `Vec` of light rig slots with `opinionated` alone: insert a light,
  duplicate the last slot, and resize with an unlit light as the host-supplied fill)
- `cargo run -p layerstack_examples --example mesh_to_usdz -- cube.usdz [arkit|generic]`
  (writes a cube with normals and UVs as a USDZ package; the default ARKit / AR Quick Look
  profile uses a USDC root layer, `generic` a USDA one)
- `cargo run -p layerstack_examples --example mesh_materials_to_usdz -- cube.usdz [arkit|generic]`
  (writes a cube whose faces are split between a textured and a constant `UsdPreviewSurface`
  material, with its textures packaged, in the same two profiles)
- `cargo run -p layerstack_examples --example scatter_to_usdz -- field.usdz [arkit|generic]`
  (scatters trees and rocks over a field as one `PointInstancer`: each prototype, with its
  materials, is written once, and each placement, given as an affine matrix and split by
  `push_affine` (mirrors become negative scales), is an index, a position, an orientation, a
  scale and an id; the `arkit` package writes the instances as references, which Apple's
  viewers draw)

`cargo run -p layerstack_examples --bin shading_values` authors a material interface
input and traces the value supplying a shader input.

The library target compiles a generated `shaderDefs.usda` matrix interface with
`#![no_std]`, covering scalar/array inputs and outputs without the standard prelude.
